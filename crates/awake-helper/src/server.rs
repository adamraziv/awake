//! Socket mechanics only. Knows nothing about what commands mean.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use awake_proto::{MAX_LINE, ProtoError, Response, encode_response, parse_request};

use crate::commands::CommandRunner;
use crate::handler::{ConnId, Handler};

pub fn bind(path: &Path) -> io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: fd is a valid open socket for the lifetime of `stream`;
    // out-pointers are valid stack locations.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

pub fn serve<R: CommandRunner + 'static>(
    listener: UnixListener,
    allowed_uid: u32,
    handler: Arc<Handler<R>>,
) -> ! {
    let next_id = AtomicU64::new(1);
    loop {
        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) => {
                eprintln!("awake-helper: accept failed: {e}");
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };
        let uid = match peer_uid(&stream) {
            Ok(u) => u,
            Err(e) => {
                eprintln!("awake-helper: getpeereid failed: {e}");
                continue;
            }
        };
        if uid != allowed_uid {
            eprintln!("awake-helper: rejected connection from uid {uid}");
            continue;
        }
        let conn = next_id.fetch_add(1, Ordering::Relaxed);
        let handler = Arc::clone(&handler);
        std::thread::spawn(move || handle_connection(stream, conn, &handler));
    }
}

pub fn handle_connection<R: CommandRunner>(stream: UnixStream, conn: ConnId, handler: &Handler<R>) {
    eprintln!("awake-helper: conn {conn} opened");
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("awake-helper: conn {conn} clone failed: {e}");
            handler.on_disconnect(conn);
            return;
        }
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    loop {
        line.clear();
        // Read at most one byte past the limit so an oversized line is detected
        // without buffering it.
        let limit = (MAX_LINE + 1) as u64;
        let n = match (&mut reader).take(limit).read_line(&mut line) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                eprintln!("awake-helper: conn {conn} read error: {e}");
                break;
            }
        };
        if n > MAX_LINE {
            eprintln!("awake-helper: conn {conn} sent oversized line, closing");
            break;
        }
        let response = match parse_request(&line) {
            Ok(req) => handler.handle(conn, req),
            Err(ProtoError::Empty) => continue,
            Err(ProtoError::TooLong) => break,
            Err(e) => Response::Err(e.to_string()),
        };
        if let Err(e) = writer.write_all(encode_response(&response).as_bytes()) {
            eprintln!("awake-helper: conn {conn} write error: {e}");
            break;
        }
    }

    handler.on_disconnect(conn);
    eprintln!("awake-helper: conn {conn} closed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;

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

    fn temp_sock() -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let n = std::process::id();
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.join(format!("awake-test-{n}-{t}.sock"))
    }

    #[test]
    fn bind_sets_world_rw_mode_and_replaces_stale_file() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_sock();
        std::fs::write(&path, b"stale").unwrap();
        let _l = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o666);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn peer_uid_matches_current_user() {
        let (a, _b) = UnixStream::pair().unwrap();
        let uid = peer_uid(&a).unwrap();
        let me = unsafe { libc::getuid() };
        assert_eq!(uid, me);
    }

    #[test]
    fn handle_connection_answers_ping_and_resets_on_eof() {
        let (client, server) = UnixStream::pair().unwrap();
        let handler = Handler::new(Recorder::default());
        let t = std::thread::spawn(move || {
            handle_connection(server, 1, &handler);
            handler
        });

        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        let mut line = String::new();

        writer.write_all(b"ping\n").unwrap();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "ok\n");

        line.clear();
        writer.write_all(b"sleep_disable\n").unwrap();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "ok\n");

        line.clear();
        writer.write_all(b"\n").unwrap(); // blank line ignored
        writer.write_all(b"reboot\n").unwrap();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "err unknown request: reboot\n");

        drop(writer);
        drop(reader);
        let handler = t.join().unwrap();
        let calls = handler.runner().calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "/usr/bin/pmset -a disablesleep 1".to_string(),
                "/usr/bin/pmset -a disablesleep 0".to_string(),
            ]
        );
        assert_eq!(handler.owner(), None);
    }

    #[test]
    fn oversized_line_closes_connection() {
        let (client, server) = UnixStream::pair().unwrap();
        let handler = Handler::new(Recorder::default());
        let t = std::thread::spawn(move || {
            handle_connection(server, 1, &handler);
        });
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        let big = vec![b'a'; MAX_LINE + 10];
        writer.write_all(&big).unwrap();
        writer.write_all(b"\n").unwrap();
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap();
        assert_eq!(n, 0, "expected EOF, got {line:?}");
        t.join().unwrap();
    }
}
