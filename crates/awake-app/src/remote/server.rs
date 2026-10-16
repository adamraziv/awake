//! Runs the HTTP server on the Tailscale IP and follows the address as it comes and goes: one
//! `awake-remote` thread with a current-thread Tokio runtime. Spec 6.1.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::auth::Auth;
use super::{PORT, router};
use crate::app::App;
use crate::power::Power;
use crate::session::SleepController;
use crate::tailscale::{self, CliIdentity};

pub const DETECT_INTERVAL: Duration = Duration::from_secs(30);
/// How long a stop waits for in-flight requests before abandoning the server task.
const STOP_GRACE: Duration = Duration::from_secs(2);

pub fn url(ip: Ipv4Addr, port: u16) -> String {
    format!("http://{ip}:{port}")
}

/// A bound server: its address, the switch that stops it, and the task to await afterwards.
pub struct Running {
    ip: Ipv4Addr,
    port: u16,
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Running {
    pub fn url(&self) -> String {
        url(self.ip, self.port)
    }
}

/// Spawns the thread. Nothing here fails loudly: a missing runtime or a failed bind is logged and
/// the menu keeps saying "Remote: off".
pub fn spawn<S, P>(app: Arc<App<S, P>>, cli: PathBuf)
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    std::thread::Builder::new()
        .name("awake-remote".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("remote: runtime not created: {e}");
                    return;
                }
            };
            rt.block_on(supervise(app, cli));
        })
        .expect("spawn remote thread");
}

/// Spec 6.1: detect on start and every 30 s. Bind when the address appears, stop when it goes,
/// rebind when it changes or the server task has died. A failed bind is retried at the next tick.
async fn supervise<S, P>(app: Arc<App<S, P>>, cli: PathBuf)
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    let mut running: Option<Running> = None;
    let mut ticker = tokio::time::interval(DETECT_INTERVAL);
    loop {
        ticker.tick().await;
        let detected = tailscale::detect_ip();
        if let Some(r) = running.as_ref()
            && r.task.is_finished()
        {
            tracing::warn!("remote: server task ended; rebinding");
            running = None;
        }
        if running.as_ref().map(|r| r.ip) == detected {
            continue;
        }
        if let Some(r) = running.take() {
            stop(r, if detected.is_some() { "address changed" } else { "Tailscale address gone" }).await;
        }
        if let Some(ip) = detected {
            running = start(app.clone(), &cli, ip, PORT).await;
        }
        app.set_remote_url(running.as_ref().map(Running::url));
    }
}

/// Binds `ip:port` and serves the router behind the CLI identity check. `None` when the bind fails.
pub async fn start<S, P>(app: Arc<App<S, P>>, cli: &Path, ip: Ipv4Addr, port: u16) -> Option<Running>
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    let addr = SocketAddr::new(ip.into(), port);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("remote: bind {addr} failed: {e}; retrying in {}s", DETECT_INTERVAL.as_secs());
            return None;
        }
    };
    let auth = Arc::new(Auth::new(Box::new(CliIdentity::new(cli)), ip));
    let service = router(app, auth).into_make_service_with_connect_info::<SocketAddr>();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let shutdown = async {
            let _ = rx.await;
        };
        if let Err(e) = axum::serve(listener, service).with_graceful_shutdown(shutdown).await {
            tracing::warn!("remote: server stopped: {e}");
        }
    });
    tracing::info!("remote: listening on {}", url(ip, port));
    Some(Running { ip, port, stop, task })
}

/// Stops accepting, lets in-flight requests finish for `STOP_GRACE`, then aborts what is left.
pub async fn stop(r: Running, why: &str) {
    tracing::info!("remote: stopping {} ({why})", r.url());
    let _ = r.stop.send(());
    let mut task = r.task;
    if tokio::time::timeout(STOP_GRACE, &mut task).await.is_err() {
        tracing::warn!("remote: server did not stop within {STOP_GRACE:?}; aborting it");
        task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::power::FakePower;
    use crate::session::{FakeSleep, Session};
    use std::io::{Read, Write};
    use std::net::TcpStream;

    /// Not 7700: the installed app may be serving there.
    const TEST_PORT: u16 = 7701;

    #[test]
    fn url_format() {
        assert_eq!(url(Ipv4Addr::new(100, 64, 0, 1), 7700), "http://100.64.0.1:7700");
    }

    fn http_get(addr: SocketAddr, path: &str) -> std::io::Result<String> {
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        write!(s, "GET {path} HTTP/1.1\r\nHost: awake\r\nConnection: close\r\n\r\n")?;
        let mut out = String::new();
        s.read_to_string(&mut out)?;
        Ok(out)
    }

    /// Binds the real Tailscale address on a test port and authorizes through the real CLI: a
    /// connection from this Mac to its own tailnet address has that address as its peer, which
    /// `whois` resolves to our own login. Passes trivially when Tailscale is down or absent.
    #[test]
    fn serves_status_on_the_tailscale_address_and_stops() {
        let (Some(ip), true) = (tailscale::detect_ip(), Path::new(tailscale::DEFAULT_CLI).exists()) else { return };
        let path = std::env::temp_dir().join("awake-remote-server-test-unused.json");
        let app = App::new(Session::new(FakeSleep::default(), true), FakePower::default(), Config::default(), path, || {});
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let running = start(app.clone(), Path::new(tailscale::DEFAULT_CLI), ip, TEST_PORT).await.expect("bind");
            assert_eq!(running.url(), format!("http://{ip}:{TEST_PORT}"));
            let addr = SocketAddr::new(ip.into(), TEST_PORT);
            // The blocking client runs on another thread so the one runtime thread can serve it.
            let reply = tokio::task::spawn_blocking(move || http_get(addr, "/api/status")).await.unwrap().unwrap();
            assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
            assert!(reply.contains("\"session\""), "{reply}");
            let page = tokio::task::spawn_blocking(move || http_get(addr, "/")).await.unwrap().unwrap();
            assert!(page.contains("<title>Awake</title>"), "{page}");
            stop(running, "test").await;
            let closed = tokio::task::spawn_blocking(move || http_get(addr, "/api/status")).await.unwrap();
            assert!(closed.is_err(), "port should be closed after stop");
        });
    }
}
