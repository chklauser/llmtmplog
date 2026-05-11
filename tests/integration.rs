use std::fs;
use std::process::Command;

fn llmtmplog_bin() -> String {
    // cargo test builds into target/debug by default
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{manifest_dir}/target/debug/llmtmplog")
}

fn extract_log_path(stderr: &str) -> String {
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("llmtmplog: stdout & stderr redirected to ") {
            return rest.to_string();
        }
    }
    panic!("could not find log path in stderr:\n{stderr}");
}

fn extract_exit_code(stderr: &str) -> i32 {
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("llmtmplog: EXIT_CODE=") {
            return rest.parse().unwrap();
        }
    }
    panic!("could not find EXIT_CODE in stderr:\n{stderr}");
}

#[test]
fn no_args_prints_help() {
    let output = Command::new(llmtmplog_bin())
        .output()
        .expect("failed to run llmtmplog");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage:"), "expected help text, got:\n{stderr}");
}

#[test]
fn basic_stdout() {
    let output = Command::new(llmtmplog_bin())
        .args(["echo", "hello world"])
        .output()
        .expect("failed to run llmtmplog");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("llmtmplog: running `echo`"));

    let log_path = extract_log_path(&stderr);
    let log_contents = fs::read_to_string(&log_path).expect("failed to read log file");
    assert_eq!(log_contents.trim(), "hello world");

    let exit_code = extract_exit_code(&stderr);
    assert_eq!(exit_code, 0);

    // No --head/--tail, so stdout should be empty
    assert!(output.stdout.is_empty(), "stdout should be empty without --head/--tail");

    fs::remove_file(&log_path).ok();
}

#[test]
fn stderr_captured() {
    let output = Command::new(llmtmplog_bin())
        .args(["sh", "-c", "echo this-is-stderr >&2"])
        .output()
        .expect("failed to run llmtmplog");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log_path = extract_log_path(&stderr);
    let log_contents = fs::read_to_string(&log_path).expect("failed to read log file");
    assert!(
        log_contents.contains("this-is-stderr"),
        "log should contain stderr output, got:\n{log_contents}"
    );

    fs::remove_file(&log_path).ok();
}

#[test]
fn nonzero_exit_code() {
    let output = Command::new(llmtmplog_bin())
        .arg("false")
        .output()
        .expect("failed to run llmtmplog");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = extract_exit_code(&stderr);
    assert_eq!(exit_code, 1);

    // The process itself should also exit with 1
    assert_eq!(output.status.code(), Some(1));

    let log_path = extract_log_path(&stderr);
    fs::remove_file(&log_path).ok();
}

#[test]
fn head_flag() {
    let output = Command::new(llmtmplog_bin())
        .args(["--head", "seq", "100"])
        .output()
        .expect("failed to run llmtmplog");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout_lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(stdout_lines.len(), 50, "expected 50 head lines, got {}", stdout_lines.len());
    assert_eq!(stdout_lines[0], "1");
    assert_eq!(stdout_lines[49], "50");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let log_path = extract_log_path(&stderr);
    let log_contents = fs::read_to_string(&log_path).expect("failed to read log file");
    let log_lines: Vec<&str> = log_contents.lines().collect();
    assert_eq!(log_lines.len(), 100, "log should have all 100 lines");
    assert_eq!(log_lines[99], "100");

    fs::remove_file(&log_path).ok();
}

#[test]
fn tail_flag() {
    let output = Command::new(llmtmplog_bin())
        .args(["--tail", "seq", "100"])
        .output()
        .expect("failed to run llmtmplog");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout_lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(stdout_lines.len(), 50, "expected 50 tail lines, got {}", stdout_lines.len());
    assert_eq!(stdout_lines[0], "51");
    assert_eq!(stdout_lines[49], "100");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let log_path = extract_log_path(&stderr);
    let log_contents = fs::read_to_string(&log_path).expect("failed to read log file");
    let log_lines: Vec<&str> = log_contents.lines().collect();
    assert_eq!(log_lines.len(), 100);

    fs::remove_file(&log_path).ok();
}

#[test]
fn head_and_tail_flags() {
    let output = Command::new(llmtmplog_bin())
        .args(["--head", "--tail", "seq", "200"])
        .output()
        .expect("failed to run llmtmplog");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout_lines: Vec<&str> = stdout.lines().collect();
    // head: lines 1-50, tail: lines 151-200 = 100 lines total
    assert_eq!(stdout_lines.len(), 100, "expected 100 lines (50 head + 50 tail), got {}", stdout_lines.len());
    assert_eq!(stdout_lines[0], "1");
    assert_eq!(stdout_lines[49], "50");
    assert_eq!(stdout_lines[50], "151");
    assert_eq!(stdout_lines[99], "200");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let log_path = extract_log_path(&stderr);
    let log_contents = fs::read_to_string(&log_path).expect("failed to read log file");
    let log_lines: Vec<&str> = log_contents.lines().collect();
    assert_eq!(log_lines.len(), 200);

    fs::remove_file(&log_path).ok();
}

#[test]
fn stdout_and_stderr_interleaved() {
    let output = Command::new(llmtmplog_bin())
        .args(["sh", "-c", "echo out1; echo err1 >&2; echo out2"])
        .output()
        .expect("failed to run llmtmplog");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log_path = extract_log_path(&stderr);
    let log_contents = fs::read_to_string(&log_path).expect("failed to read log file");
    assert!(log_contents.contains("out1"), "log should contain out1");
    assert!(log_contents.contains("err1"), "log should contain err1");
    assert!(log_contents.contains("out2"), "log should contain out2");

    fs::remove_file(&log_path).ok();
}
