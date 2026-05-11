use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio, exit};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

const HEAD_LINES: usize = 50;
const TAIL_LINES: usize = 50;
const BUF_SIZE: usize = 8192;
const MAX_LINE_DISPLAY: usize = 4096;

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
    let mut log_file = File::create(&log_path).expect("failed to create log file");

    eprintln!("llmtmplog: stdout & stderr redirected to {}", log_path.display());
    let _ = io::stderr().flush();

    let mut child = Command::new(program)
        .args(&cmd_args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("llmtmplog: failed to spawn `{program}`: {e}");
            exit(127);
        });

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let tx2 = tx.clone();

    let t1 = thread::spawn(move || pipe_reader(stdout, tx));
    let t2 = thread::spawn(move || pipe_reader(stderr, tx2));

    let mut tracker = LineTracker::new(show_head, show_tail);

    for chunk in rx {
        log_file.write_all(&chunk).expect("failed to write to log");
        tracker.feed(&chunk);
    }

    t1.join().unwrap();
    t2.join().unwrap();

    let status = child.wait().expect("failed to wait on child");
    let code = status.code().unwrap_or(128);

    tracker.flush_tail();

    eprintln!("llmtmplog: EXIT_CODE={code}");
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
