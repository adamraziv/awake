use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use awake_helper::commands::CommandRunner;
use awake_helper::handler::Handler;
use awake_helper::server;

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<String>>,
}

impl CommandRunner for Recorder {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<()> {
        self.calls.lock().unwrap().push(format!("{program} {}", args.join(" ")));
        Ok(())
    }
}

fn temp_sock(tag: &str) -> std::path::PathBuf {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("awake-it-{tag}-{}-{t}.sock", std::process::id()))
}

fn start(allowed_uid: u32, tag: &str) -> (std::path::PathBuf, Arc<Handler<Recorder>>) {
    let path = temp_sock(tag);
    let listener = server::bind(&path).unwrap();
    let handler = Arc::new(Handler::new(Recorder::default()));
    let h = Arc::clone(&handler);
    std::thread::spawn(move || server::serve(listener, allowed_uid, h));
    (path, handler)
}

fn wait_for<F: Fn() -> bool>(desc: &str, f: F) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {desc}");
}

fn me() -> u32 {
    unsafe { libc::getuid() }
}

#[test]
fn full_session_then_disconnect_resets_sleep() {
    let (path, handler) = start(me(), "session");
    let stream = UnixStream::connect(&path).unwrap();
    let mut w = stream.try_clone().unwrap();
    let mut r = BufReader::new(stream);
    let mut line = String::new();

    w.write_all(b"sleep_disable\n").unwrap();
    r.read_line(&mut line).unwrap();
    assert_eq!(line, "ok\n");
    assert_eq!(handler.owner(), Some(1));

    drop(w);
    drop(r);

    wait_for("owner cleared after disconnect", || handler.owner().is_none());
    let calls = handler.runner().calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            "/usr/bin/pmset -a disablesleep 1".to_string(),
            "/usr/bin/pmset -a disablesleep 0".to_string(),
        ]
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn second_connection_gets_busy_while_first_owns() {
    let (path, handler) = start(me(), "busy");
    let first = UnixStream::connect(&path).unwrap();
    let mut w1 = first.try_clone().unwrap();
    let mut r1 = BufReader::new(first);
    let mut line = String::new();
    w1.write_all(b"sleep_disable\n").unwrap();
    r1.read_line(&mut line).unwrap();
    assert_eq!(line, "ok\n");

    let second = UnixStream::connect(&path).unwrap();
    let mut w2 = second.try_clone().unwrap();
    let mut r2 = BufReader::new(second);
    line.clear();
    w2.write_all(b"sleep_disable\n").unwrap();
    r2.read_line(&mut line).unwrap();
    assert_eq!(line, "busy\n");

    line.clear();
    w2.write_all(b"ping\n").unwrap();
    r2.read_line(&mut line).unwrap();
    assert_eq!(line, "ok\n");

    assert_eq!(handler.owner(), Some(1));
    std::fs::remove_file(&path).ok();
}

#[test]
fn wrong_uid_is_dropped_without_reply() {
    let (path, handler) = start(me() + 1, "uid");
    let stream = UnixStream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut w = stream.try_clone().unwrap();
    let mut r = BufReader::new(stream);
    let _ = w.write_all(b"ping\n");
    let mut line = String::new();
    let result = r.read_line(&mut line);
    match result {
        Ok(0) => {}
        Ok(n) => panic!("expected EOF, got {n} bytes: {line:?}"),
        Err(e) => panic!("expected clean EOF, got error {e}"),
    }
    assert_eq!(handler.owner(), None);
    assert!(handler.runner().calls.lock().unwrap().is_empty());
    std::fs::remove_file(&path).ok();
}
