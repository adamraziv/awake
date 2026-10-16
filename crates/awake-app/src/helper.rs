//! Client for the root helper socket. Spec sections 3.2 and 4.5.
//!
//! Blocking I/O with timeouts. The helper keeps every connection open and answers one line
//! per request, so a request is one write followed by one `read_line`.

use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use awake_proto::{Request, Response, encode_request, parse_response};

pub const SOCKET_PATH: &str = "/var/run/awake-helper.sock";
pub const IO_TIMEOUT: Duration = Duration::from_secs(2);
pub const PING_INTERVAL: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum HelperError {
    NotConnected,
    Io(io::Error),
    Busy,
    Rejected(String),
    Protocol(String),
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HelperError::NotConnected => write!(f, "helper not connected"),
            HelperError::Io(e) => write!(f, "helper I/O error: {e}"),
            HelperError::Busy => write!(f, "helper busy (another owner)"),
            HelperError::Rejected(m) => write!(f, "helper refused: {m}"),
            HelperError::Protocol(m) => write!(f, "helper protocol error: {m}"),
        }
    }
}

impl std::error::Error for HelperError {}

type Conn = BufReader<UnixStream>;

pub struct HelperClient {
    path: PathBuf,
    conn: Mutex<Option<Conn>>,
}

impl HelperClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), conn: Mutex::new(None) }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Conn>> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn is_connected(&self) -> bool {
        self.lock().is_some()
    }

    /// Open the socket. Ok(()) if already connected.
    pub fn connect(&self) -> io::Result<()> {
        let mut guard = self.lock();
        if guard.is_some() {
            return Ok(());
        }
        let stream = UnixStream::connect(&self.path)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        *guard = Some(BufReader::new(stream));
        Ok(())
    }

    pub fn disconnect(&self) {
        *self.lock() = None;
    }

    /// One request, one reply. Any transport failure drops the connection so the keeper
    /// thread reconnects; protocol-level `busy` and `err` replies keep it open.
    pub fn send(&self, req: Request) -> Result<Response, HelperError> {
        let mut guard = self.lock();
        let Some(conn) = guard.as_mut() else {
            return Err(HelperError::NotConnected);
        };
        let result = exchange(conn, req);
        if matches!(result, Err(HelperError::Io(_)) | Err(HelperError::Protocol(_))) {
            *guard = None;
        }
        result
    }

    /// `ok` becomes Ok(()); `busy` and `err <m>` become errors.
    pub fn request_ok(&self, req: Request) -> Result<(), HelperError> {
        match self.send(req)? {
            Response::Ok => Ok(()),
            Response::Busy => Err(HelperError::Busy),
            Response::Err(m) => Err(HelperError::Rejected(m)),
        }
    }
}

fn exchange(conn: &mut Conn, req: Request) -> Result<Response, HelperError> {
    conn.get_mut()
        .write_all(encode_request(req).as_bytes())
        .map_err(HelperError::Io)?;
    let mut line = String::new();
    let n = conn.read_line(&mut line).map_err(HelperError::Io)?;
    if n == 0 {
        return Err(HelperError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "helper closed the connection",
        )));
    }
    parse_response(&line).map_err(|e| HelperError::Protocol(e.to_string()))
}

/// 1 s, 2 s, 4 s, 8 s, 16 s, then 30 s forever (spec 4.5).
pub fn backoff(attempt: u32) -> Duration {
    let secs = 1u64 << attempt.min(5);
    Duration::from_secs(secs).min(MAX_BACKOFF)
}

/// What the keeper thread tells the rest of the app.
pub trait HelperEvents: Send + Sync + 'static {
    fn connected(&self);
    fn lost(&self);
}

/// Keeps the helper connection alive for the app's lifetime: connect with backoff, then ping
/// every `ping_interval` until a ping fails, report, and start over. Never returns.
pub fn spawn_keeper(
    client: Arc<HelperClient>,
    events: Arc<dyn HelperEvents>,
    ping_interval: Duration,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("awake-helper-keeper".into())
        .spawn(move || {
            let mut attempt: u32 = 0;
            loop {
                match client.connect() {
                    Ok(()) => {
                        attempt = 0;
                        tracing::info!("helper connected");
                        events.connected();
                        loop {
                            std::thread::sleep(ping_interval);
                            if client.send(Request::Ping).is_err() {
                                break;
                            }
                        }
                        client.disconnect();
                        tracing::warn!("helper connection lost");
                        events.lost();
                    }
                    Err(e) => {
                        if attempt == 0 {
                            tracing::warn!("helper not reachable: {e}");
                        }
                        let wait = backoff(attempt);
                        attempt = attempt.saturating_add(1);
                        std::thread::sleep(wait);
                    }
                }
            }
        })
        .expect("spawn helper keeper thread")
}
