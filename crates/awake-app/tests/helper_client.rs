//! HelperClient against a fake helper speaking the awake-proto line protocol on a temp socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use awake_app::helper::{HelperClient, HelperError, HelperEvents, backoff, spawn_keeper};
use awake_proto::{Request, Response, encode_response, parse_request};

fn temp_sock(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("awake-hc-{name}-{}-{nanos}.sock", std::process::id()))
}

/// Replies `ok` to everything except: `sleep_disable` gets `busy` while `busy` is set,
/// and `shutdown` gets `err refused`. Closes the client when `hangup` is set.
struct FakeHelper {
    busy: Arc<AtomicBool>,
    hangup: Arc<AtomicBool>,
}

fn serve_one(stream: UnixStream, fake: &FakeHelper) {
    let mut writer = stream.try_clone().unwrap();
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if fake.hangup.load(Ordering::SeqCst) {
            break;
        }
        let resp = match parse_request(&line) {
            Ok(Request::SleepDisable) if fake.busy.load(Ordering::SeqCst) => Response::Busy,
            Ok(Request::Shutdown) => Response::Err("refused".into()),
            Ok(_) => Response::Ok,
            Err(e) => Response::Err(e.to_string()),
        };
        if writer.write_all(encode_response(&resp).as_bytes()).is_err() {
            break;
        }
    }
}

fn start_fake(path: &PathBuf, fake: FakeHelper) -> UnixListener {
    let listener = UnixListener::bind(path).unwrap();
    let accept = listener.try_clone().unwrap();
    std::thread::spawn(move || {
        for stream in accept.incoming() {
            let Ok(stream) = stream else { break };
            serve_one(stream, &fake);
        }
    });
    listener
}

fn flags() -> (Arc<AtomicBool>, Arc<AtomicBool>) {
    (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)))
}

#[test]
fn not_connected_before_connect() {
    let client = HelperClient::new(temp_sock("none"));
    assert!(!client.is_connected());
    assert!(matches!(client.send(Request::Ping), Err(HelperError::NotConnected)));
}

#[test]
fn connect_fails_when_socket_missing() {
    let client = HelperClient::new(temp_sock("missing"));
    assert!(client.connect().is_err());
    assert!(!client.is_connected());
}

#[test]
fn ping_round_trips_and_errors_map() {
    let path = temp_sock("rt");
    let (busy, hangup) = flags();
    let _l = start_fake(&path, FakeHelper { busy: busy.clone(), hangup });
    let client = HelperClient::new(&path);
    client.connect().unwrap();
    assert!(client.is_connected());
    assert_eq!(client.send(Request::Ping).unwrap(), Response::Ok);
    assert!(client.request_ok(Request::SleepDisable).is_ok());
    busy.store(true, Ordering::SeqCst);
    assert!(matches!(client.request_ok(Request::SleepDisable), Err(HelperError::Busy)));
    match client.request_ok(Request::Shutdown) {
        Err(HelperError::Rejected(m)) => assert_eq!(m, "refused"),
        other => panic!("expected Rejected, got {other:?}"),
    }
    // Still connected after protocol-level refusals.
    assert!(client.is_connected());
}

#[test]
fn server_hangup_drops_connection() {
    let path = temp_sock("hangup");
    let (busy, hangup) = flags();
    let _l = start_fake(&path, FakeHelper { busy, hangup: hangup.clone() });
    let client = HelperClient::new(&path);
    client.connect().unwrap();
    hangup.store(true, Ordering::SeqCst);
    assert!(matches!(client.send(Request::Ping), Err(HelperError::Io(_))));
    assert!(!client.is_connected());
    assert!(matches!(client.send(Request::Ping), Err(HelperError::NotConnected)));
}

#[test]
fn backoff_doubles_and_caps_at_30s() {
    let secs: Vec<u64> = (0..8).map(|a| backoff(a).as_secs()).collect();
    assert_eq!(secs, vec![1, 2, 4, 8, 16, 30, 30, 30]);
}

struct Recorder(mpsc::Sender<&'static str>);

impl HelperEvents for Recorder {
    fn connected(&self) {
        let _ = self.0.send("connected");
    }
    fn lost(&self) {
        let _ = self.0.send("lost");
    }
}

#[test]
fn keeper_connects_when_socket_appears_and_reports_loss() {
    let path = temp_sock("keeper");
    let client = Arc::new(HelperClient::new(&path));
    let (tx, rx) = mpsc::channel();
    let _h = spawn_keeper(client.clone(), Arc::new(Recorder(tx)), Duration::from_millis(100));

    // No socket yet: the keeper must keep retrying without reporting anything.
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());

    let (busy, hangup) = flags();
    let listener = start_fake(&path, FakeHelper { busy, hangup: hangup.clone() });
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "connected");
    assert!(client.is_connected());

    // Kill the server: pings fail, keeper reports loss, and keeps retrying.
    hangup.store(true, Ordering::SeqCst);
    drop(listener);
    std::fs::remove_file(&path).ok();
    let t0 = Instant::now();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "lost");
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert!(!client.is_connected());
}
