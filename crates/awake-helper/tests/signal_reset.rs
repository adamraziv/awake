//! SIGTERM to the helper process resets sleep before it exits.
//!
//! Runs without the libtest harness: libtest's own threads would not block the signal, so a
//! process-directed SIGTERM could land on one of them and kill the process before `sigwait`
//! sees it. This binary re-runs itself as a child that, like the helper, installs the reset
//! from `main` before any other thread exists. The parent sends SIGTERM the way launchd does.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use awake_helper::commands::CommandRunner;
use awake_helper::signals::reset_on_termination;

const CHILD_LOG_ENV: &str = "AWAKE_SIGNAL_TEST_LOG";

/// Appends each command to a file, because the child exits before it could report any other way.
struct FileRecorder(PathBuf);

impl CommandRunner for FileRecorder {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.0)?;
        writeln!(f, "{program} {}", args.join(" "))
    }
}

fn child(log: PathBuf) -> ! {
    reset_on_termination(FileRecorder(log)).expect("install signal reset");
    println!("ready");
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(10));
    eprintln!("no signal arrived");
    std::process::exit(2);
}

fn sigterm_resets_sleep_then_exits_zero() {
    let log = std::env::temp_dir().join(format!("awake-signal-{}.log", std::process::id()));
    std::fs::remove_file(&log).ok();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .env(CHILD_LOG_ENV, &log)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");

    // SAFETY: plain kill(2) on a pid this process owns and has not reaped.
    assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) }, 0);

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "child did not exit after SIGTERM");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(0), "the reset path exits 0; a default SIGTERM death has no code");
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "/usr/bin/pmset -a disablesleep 0\n");
    std::fs::remove_file(&log).ok();
}

fn main() {
    if let Some(log) = std::env::var_os(CHILD_LOG_ENV) {
        child(PathBuf::from(log));
    }
    sigterm_resets_sleep_then_exits_zero();
    println!("test sigterm_resets_sleep_then_exits_zero ... ok");
}
