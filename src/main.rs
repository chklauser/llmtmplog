use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio, exit};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const HEAD_LINES: usize = 50;
const TAIL_LINES: usize = 50;
const BUF_SIZE: usize = 8192;
const MAX_LINE_DISPLAY: usize = 4096;
const LOG_RETENTION: Duration = Duration::from_secs(24 * 3600);

fn log_dir() -> PathBuf {
    dirs::cache_dir()
        .expect("could not determine a cache directory for this platform")
        .join("llmtmplog")
}

fn print_help() {
    let dir = log_dir();
    eprintln!("Usage: llmtmplog [--head] [--tail] <command> [args...]");
    eprintln!();
    eprintln!("Runs <command> and redirects stdout+stderr to a unique log file");
    eprintln!("in {}/.", dir.display());
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --head  Print the first {HEAD_LINES} lines to stdout (streamed)");
    eprintln!("  --tail  Print the last {TAIL_LINES} lines to stdout (after command exits)");
    eprintln!();
    eprintln!("The log file path is printed immediately so you can tail -f it.");
    eprintln!("llmtmplog exits with the same exit code as <command>.");
}

fn generate_log_path(dir: &Path) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = now.as_secs();
    let nanos = now.subsec_nanos();

    // Convert to YYYYMMDD-HHMMSS (UTC)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);

    // Sub-second component + pid disambiguates concurrent invocations.
    let pid = std::process::id();

    dir.join(format!(
        "{year:04}{month:02}{day:02}-{hours:02}{minutes:02}{seconds:02}-{nanos:09}-{pid}.log"
    ))
}

// Matches the filenames produced by `generate_log_path`:
// `YYYYMMDD-HHMMSS-NNNNNNNNN-PID.log`.
fn is_temp_log_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".log") else {
        return false;
    };
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() != 4 {
        return false;
    }
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    parts[0].len() == 8
        && all_digits(parts[0])
        && parts[1].len() == 6
        && all_digits(parts[1])
        && parts[2].len() == 9
        && all_digits(parts[2])
        && all_digits(parts[3])
}

fn gc_sweep(dir: &Path, stop: &AtomicBool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if !is_temp_log_name(name_str) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else { continue };
        let Ok(modified) = metadata.modified() else { continue };
        let Ok(age) = now.duration_since(modified) else { continue };
        if age >= LOG_RETENTION {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn spawn_gc(dir: PathBuf) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    thread::spawn(move || gc_sweep(&dir, &stop_for_thread));
    stop
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

struct LineTracker {
    show_head: bool,
    show_tail: bool,
    head_remaining: usize,
    tail_buf: VecDeque<Vec<u8>>,
    partial_line: Vec<u8>,
}

impl LineTracker {
    fn new(show_head: bool, show_tail: bool) -> Self {
        Self {
            show_head,
            show_tail,
            head_remaining: HEAD_LINES,
            tail_buf: VecDeque::with_capacity(TAIL_LINES + 1),
            partial_line: Vec::new(),
        }
    }

    fn active(&self) -> bool {
        self.show_head || self.show_tail
    }

    fn feed(&mut self, data: &[u8]) {
        if !self.active() {
            return;
        }

        let mut start = 0;
        for i in 0..data.len() {
            if data[i] == b'\n' {
                self.partial_line.extend_from_slice(&data[start..=i]);
                self.finish_line();
                start = i + 1;
            }
        }
        if start < data.len() {
            self.partial_line.extend_from_slice(&data[start..]);
            // If partial line is getting huge, flush it as a "line"
            if self.partial_line.len() > MAX_LINE_DISPLAY * 2 {
                self.finish_line();
            }
        }
    }

    fn finish_line(&mut self) {
        let line = std::mem::take(&mut self.partial_line);

        if self.show_head && self.head_remaining > 0 {
            self.head_remaining -= 1;
            emit_line(&line);
        }

        if self.show_tail {
            if self.tail_buf.len() == TAIL_LINES {
                self.tail_buf.pop_front();
            }
            self.tail_buf.push_back(line);
        }
    }

    fn flush_tail(&mut self) {
        // Flush any remaining partial line
        if !self.partial_line.is_empty() {
            self.finish_line();
        }

        if self.show_tail {
            for line in &self.tail_buf {
                emit_line(line);
            }
        }
    }
}

fn emit_line(line: &[u8]) {
    let display = if line.len() > MAX_LINE_DISPLAY {
        &line[..MAX_LINE_DISPLAY]
    } else {
        line
    };
    let s = String::from_utf8_lossy(display);
    // The line already contains \n if it was newline-terminated
    if s.ends_with('\n') {
        print!("{s}");
    } else {
        println!("{s}");
    }
    let _ = io::stdout().flush();
}

fn pipe_reader(mut pipe: impl Read + Send + 'static, tx: mpsc::Sender<Vec<u8>>) {
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

// Holds the pgid (== pid) of the child process group while it is running, so
// signal handlers can forward signals to it. 0 means "no child yet" or "child
// already reaped"; we treat any non-positive value as "do nothing".
#[cfg(unix)]
static CHILD_PGID: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn forward_signal(sig: libc::c_int) {
    let pgid = CHILD_PGID.load(Ordering::SeqCst);
    if pgid > 0 {
        // kill() and atomic load are async-signal-safe.
        unsafe {
            libc::kill(-pgid, sig);
        }
    }
}

#[cfg(unix)]
fn install_signal_forwarders() {
    let handler = forward_signal as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
        libc::signal(libc::SIGQUIT, handler);
    }
}

#[cfg(unix)]
fn configure_child_process_group(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            // setpgid(0, 0) puts the about-to-exec process into a new process
            // group whose id equals its pid. Done after fork, before exec, in
            // the outer namespace — so the pgid covers any grandchildren the
            // command later spawns (including across bwrap PID-namespace
            // unsharing).
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn install_signal_forwarders() {}

#[cfg(not(unix))]
fn configure_child_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(pgid: i32) {
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pgid: i32) {}

fn run(args: Vec<String>) -> i32 {
    let mut show_head = false;
    let mut show_tail = false;
    let mut cmd_start = 0;

    // Parse flags in order: [--head] [--tail] <command...>
    if args.get(cmd_start).is_some_and(|a| a == "--head") {
        show_head = true;
        cmd_start += 1;
    }
    if args.get(cmd_start).is_some_and(|a| a == "--tail") {
        show_tail = true;
        cmd_start += 1;
    }

    let cmd_args = &args[cmd_start..];
    if cmd_args.is_empty() {
        print_help();
        return 1;
    }

    let program = &cmd_args[0];
    let dir = log_dir();
    let log_path = generate_log_path(&dir);

    eprintln!("llmtmplog: running `{program}`");

    fs::create_dir_all(&dir).expect("failed to create log directory");

    let gc_stop = spawn_gc(dir.clone());

    let mut log_file = File::create(&log_path).expect("failed to create log file");

    eprintln!("llmtmplog: stdout & stderr redirected to {}", log_path.display());
    let _ = io::stderr().flush();

    let mut cmd = Command::new(program);
    cmd.args(&cmd_args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_child_process_group(&mut cmd);

    let mut child = cmd.spawn().unwrap_or_else(|e| {
        eprintln!("llmtmplog: failed to spawn `{program}`: {e}");
        exit(127);
    });

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // The child was placed in a new process group whose id equals its pid
    // (see configure_child_process_group). Publish it so signal handlers can
    // forward signals to the group, and then install the handlers.
    let child_pid = child.id() as i32;
    #[cfg(unix)]
    CHILD_PGID.store(child_pid, Ordering::SeqCst);
    install_signal_forwarders();

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let tx2 = tx.clone();

    let t1 = thread::spawn(move || pipe_reader(stdout, tx));
    let t2 = thread::spawn(move || pipe_reader(stderr, tx2));

    // Wait on the child in its own thread. Once it has exited, kill the rest
    // of its process group with SIGKILL so any orphaned grandchildren (which
    // inherited stdout/stderr) release the pipe, letting the readers see EOF
    // and unblocking the drain loop below.
    let (status_tx, status_rx) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let status = child.wait();
        kill_process_group(child_pid);
        #[cfg(unix)]
        CHILD_PGID.store(0, Ordering::SeqCst);
        let _ = status_tx.send(status);
    });

    let mut tracker = LineTracker::new(show_head, show_tail);

    for chunk in rx {
        log_file.write_all(&chunk).expect("failed to write to log");
        tracker.feed(&chunk);
    }

    t1.join().unwrap();
    t2.join().unwrap();
    waiter.join().unwrap();

    let status = status_rx.recv().expect("waiter thread dropped status").expect("failed to wait on child");
    let code = status.code().unwrap_or(128);

    tracker.flush_tail();

    eprintln!("llmtmplog: EXIT_CODE={code}");

    // Cooperative shutdown: ask the GC thread to stop checking new entries.
    // We don't join — the goal is to never make the user wait for GC.
    gc_stop.store(true, Ordering::Relaxed);

    code
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        print_help();
        exit(1);
    }

    if matches!(args[0].as_str(), "-h" | "--help") {
        print_help();
        exit(0);
    }

    let code = run(args);
    exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_log_name_matches_generated_format() {
        assert!(is_temp_log_name("20260511-172309-737498212-91313.log"));
        assert!(is_temp_log_name("20260101-000000-000000000-1.log"));
        assert!(is_temp_log_name("99991231-235959-999999999-4194304.log"));
    }

    #[test]
    fn temp_log_name_rejects_other_names() {
        assert!(!is_temp_log_name(""));
        assert!(!is_temp_log_name("foo.log"));
        assert!(!is_temp_log_name("notes.txt"));
        assert!(!is_temp_log_name("20260511-172309-737498212-91313.txt"));
        assert!(!is_temp_log_name("20260511-172309-737498212.log"));
        assert!(!is_temp_log_name("2026051-172309-737498212-91313.log"));
        assert!(!is_temp_log_name("20260511-17239-737498212-91313.log"));
        assert!(!is_temp_log_name("20260511-172309-73749821-91313.log"));
        assert!(!is_temp_log_name("20260511-172309-737498212-.log"));
        assert!(!is_temp_log_name("20260511-172309-abcdefghi-91313.log"));
        assert!(!is_temp_log_name("20260511-172309-737498212-91313-extra.log"));
    }

    #[test]
    fn generated_log_name_is_recognised() {
        let path = generate_log_path(Path::new("/tmp"));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(is_temp_log_name(name), "generated name not recognised: {name}");
    }

    fn unique_test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "llmtmplog-gc-test-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path, age: Duration) {
        File::create(path).unwrap();
        let mtime = SystemTime::now() - age;
        let f = File::options().write(true).open(path).unwrap();
        f.set_modified(mtime).unwrap();
    }

    #[test]
    fn gc_sweep_deletes_old_temp_logs_only() {
        let dir = unique_test_dir("sweep");
        let old_log = dir.join("20260101-000000-000000000-1.log");
        let fresh_log = dir.join("20260102-000000-000000000-2.log");
        let other_old = dir.join("notes.txt");
        let other_pattern = dir.join("not-a-log.log");
        touch(&old_log, Duration::from_secs(25 * 3600));
        touch(&fresh_log, Duration::from_secs(60));
        touch(&other_old, Duration::from_secs(48 * 3600));
        touch(&other_pattern, Duration::from_secs(48 * 3600));

        gc_sweep(&dir, &AtomicBool::new(false));

        assert!(!old_log.exists(), "old temp log should be deleted");
        assert!(fresh_log.exists(), "fresh temp log should be kept");
        assert!(other_old.exists(), "non-temp file should be kept");
        assert!(other_pattern.exists(), "file not matching pattern should be kept");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gc_sweep_honours_stop_flag() {
        let dir = unique_test_dir("stop");
        let log = dir.join("20260101-000000-000000000-1.log");
        touch(&log, Duration::from_secs(48 * 3600));

        let stop = AtomicBool::new(true);
        gc_sweep(&dir, &stop);

        assert!(log.exists(), "stopped sweep should not delete anything");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gc_sweep_tolerates_missing_dir() {
        let dir = std::env::temp_dir().join(format!(
            "llmtmplog-gc-test-missing-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Directory deliberately not created.
        gc_sweep(&dir, &AtomicBool::new(false));
    }
}
