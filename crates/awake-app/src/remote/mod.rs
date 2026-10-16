//! Remote control over HTTP on the Tailscale IP. Spec section 6. `auth.rs` decides who may call,
//! `status.rs` shapes `/api/status`, `server.rs` runs the server; the router and handlers live here.
//! Handlers only talk to `App`, through `spawn_blocking` because `App` can block on the helper
//! socket (start, stop, shutdown) or on `pmset` (screen off, lock).

pub mod auth;
pub mod server;
pub mod status;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{StatusCode, header::CONTENT_TYPE};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;

use crate::app::App;
use crate::power::Power;
use crate::session::{EndReason, SleepController};
use auth::Auth;

pub const PORT: u16 = 7700;
/// Spec 6.3 gives no upper bound; a day keeps a typo like 6000 from meaning four days.
pub const MAX_MINUTES: u64 = 24 * 60;
/// The whole page. Editing `web/index.html` needs a rebuild; nothing reads it at run time.
pub const INDEX_HTML: &str = include_str!("../../web/index.html");

/// Shared by every handler. Cloning is two `Arc` bumps.
pub struct Ctx<S: SleepController, P: Power> {
    pub app: Arc<App<S, P>>,
    pub auth: Arc<Auth>,
}

impl<S: SleepController, P: Power> Clone for Ctx<S, P> {
    fn clone(&self) -> Self {
        Self { app: self.app.clone(), auth: self.auth.clone() }
    }
}

/// Every route, including the 404 fallback, sits behind `require_owner`.
pub fn router<S, P>(app: Arc<App<S, P>>, auth: Arc<Auth>) -> Router
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    let ctx = Ctx { app, auth };
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(get_status::<S, P>))
        .route("/api/session/start", post(start::<S, P>))
        .route("/api/session/stop", post(stop::<S, P>))
        .route("/api/screen/off", post(screen_off::<S, P>))
        .route("/api/lock", post(lock::<S, P>))
        .route("/api/shutdown", post(shutdown::<S, P>))
        .layer(middleware::from_fn_with_state(ctx.clone(), require_owner::<S, P>))
        .with_state(ctx)
}

/// Spec 6.2: the peer's login must equal ours; any failure is 403.
async fn require_owner<S, P>(
    State(ctx): State<Ctx<S, P>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    let auth = ctx.auth.clone();
    let ip: IpAddr = peer.ip();
    let allowed = blocking(move || auth.allows(ip, Instant::now())).await.unwrap_or(false);
    if allowed {
        next.run(req).await
    } else {
        tracing::warn!("remote: refused {ip} {} {}", req.method(), req.uri().path());
        (StatusCode::FORBIDDEN, "forbidden").into_response()
    }
}

/// `App` calls can block on the helper socket or `pmset`; keep them off the runtime thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::error!("remote: blocking task failed: {e}");
            None
        }
    }
}

fn json<T: Serialize>(code: StatusCode, value: &T) -> Response {
    match serde_json::to_string(value) {
        Ok(body) => (code, [(CONTENT_TYPE, "application/json")], body).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Serialize)]
struct Done {
    ok: bool,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

fn ok() -> Response {
    json(StatusCode::OK, &Done { ok: true })
}

fn error(code: StatusCode, message: &str) -> Response {
    json(code, &ErrorBody { error: message })
}

fn status_response<S, P>(ctx: &Ctx<S, P>) -> Response
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    let model = ctx.app.menu_model(Instant::now());
    let metrics = ctx.app.metrics();
    json(StatusCode::OK, &status::build(&model, metrics.as_ref(), SystemTime::now()))
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn get_status<S, P>(State(ctx): State<Ctx<S, P>>) -> Response
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    status_response(&ctx)
}

#[derive(serde::Deserialize)]
struct StartBody {
    minutes: Option<u64>,
}

async fn start<S, P>(State(ctx): State<Ctx<S, P>>, body: Bytes) -> Response
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    const HINT: &str = "body must be {\"minutes\": 1..=1440} or {\"minutes\": null}";
    let minutes = match serde_json::from_slice::<StartBody>(&body) {
        Ok(StartBody { minutes: Some(m) }) if !(1..=MAX_MINUTES).contains(&m) => {
            return error(StatusCode::BAD_REQUEST, HINT);
        }
        Ok(StartBody { minutes }) => minutes,
        Err(_) => return error(StatusCode::BAD_REQUEST, HINT),
    };
    tracing::info!("remote: start session, minutes {minutes:?}");
    let app = ctx.app.clone();
    if blocking(move || app.start_session(minutes)).await.is_none() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "start failed");
    }
    status_response(&ctx)
}

async fn stop<S, P>(State(ctx): State<Ctx<S, P>>) -> Response
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    tracing::info!("remote: stop session");
    let app = ctx.app.clone();
    if blocking(move || app.stop_session(EndReason::Remote)).await.is_none() {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "stop failed");
    }
    status_response(&ctx)
}

async fn screen_off<S, P>(State(ctx): State<Ctx<S, P>>) -> Response
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    tracing::info!("remote: screen off");
    let app = ctx.app.clone();
    match blocking(move || app.screen_off()).await {
        Some(()) => ok(),
        None => error(StatusCode::INTERNAL_SERVER_ERROR, "screen off failed"),
    }
}

async fn lock<S, P>(State(ctx): State<Ctx<S, P>>) -> Response
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    tracing::info!("remote: lock");
    let app = ctx.app.clone();
    match blocking(move || app.lock_screen()).await {
        Some(()) => ok(),
        None => error(StatusCode::INTERNAL_SERVER_ERROR, "lock failed"),
    }
}

#[derive(serde::Deserialize)]
struct ShutdownBody {
    confirm: bool,
}

async fn shutdown<S, P>(State(ctx): State<Ctx<S, P>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, body: Bytes) -> Response
where
    S: SleepController + Send + 'static,
    P: Power + 'static,
{
    if !matches!(serde_json::from_slice::<ShutdownBody>(&body), Ok(ShutdownBody { confirm: true })) {
        return error(StatusCode::BAD_REQUEST, "confirm must be true");
    }
    tracing::warn!("remote: shutdown confirmed by {}", peer.ip());
    let app = ctx.app.clone();
    match blocking(move || app.shutdown()).await {
        Some(Ok(())) => ok(),
        Some(Err(e)) => error(StatusCode::SERVICE_UNAVAILABLE, &e),
        None => error(StatusCode::INTERNAL_SERVER_ERROR, "shutdown failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::power::FakePower;
    use crate::session::{FakeSleep, Session};
    use crate::tailscale::FakeIdentity;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::net::Ipv4Addr;
    use tower::ServiceExt;

    type TestApp = App<FakeSleep, FakePower>;
    const ME: &str = "me@example.com";
    const SELF: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 1);
    const OWNER: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 2);
    const STRANGER: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 3);
    const UNKNOWN: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 4);

    fn harness_with(sleep: FakeSleep, identities: &[(IpAddr, &str)]) -> (Arc<TestApp>, Router) {
        // Never written: no test here changes a preference.
        let path = std::env::temp_dir().join("awake-remote-tests-unused.json");
        let app = App::new(Session::new(sleep, true), FakePower::default(), Config::default(), path, || {});
        let auth = Arc::new(Auth::new(Box::new(FakeIdentity::with(identities)), SELF));
        (app.clone(), router(app, auth))
    }

    fn harness(sleep: FakeSleep) -> (Arc<TestApp>, Router) {
        harness_with(sleep, &[(SELF.into(), ME), (OWNER.into(), ME), (STRANGER.into(), "other@example.com")])
    }

    fn request(method: &str, path: &str, body: &str, peer: Ipv4Addr) -> Request {
        let mut req = Request::builder().method(method).uri(path).body(Body::from(body.to_string())).unwrap();
        // What `into_make_service_with_connect_info` would insert for a real connection.
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(peer.into(), 40000)));
        req
    }

    /// One request on a fresh current-thread runtime (handlers use `spawn_blocking`, which needs one).
    fn call(router: &Router, method: &str, path: &str, body: &str, peer: Ipv4Addr) -> (StatusCode, String, String) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let resp = router.clone().oneshot(request(method, path, body, peer)).await.unwrap();
            let status = resp.status();
            let content_type =
                resp.headers().get(CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (status, content_type, String::from_utf8_lossy(&bytes).into_owned())
        })
    }

    fn json(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }

    fn sleep_calls(app: &TestApp) -> Vec<&'static str> {
        app.with_session(|s| s.sleep().calls.clone())
    }

    #[test]
    fn page_is_embedded_and_self_contained() {
        assert!(INDEX_HTML.contains("<title>Awake</title>"));
        assert!(INDEX_HTML.contains("prefers-color-scheme"));
        assert!(INDEX_HTML.contains("viewport"));
        assert!(!INDEX_HTML.contains("<script src="), "no external scripts");
        assert!(!INDEX_HTML.contains("<link "), "no external stylesheets");
        for path in ["/api/status", "/api/session/start", "/api/session/stop", "/api/screen/off", "/api/lock", "/api/shutdown"] {
            assert!(INDEX_HTML.contains(path), "{path}");
        }
        for minutes in crate::session::PRESET_MINUTES {
            assert!(INDEX_HTML.contains(&format!("data-minutes=\"{minutes}\"")), "preset {minutes}");
        }
    }

    #[test]
    fn owner_gets_page_and_others_get_403() {
        let (_, r) = harness(FakeSleep::default());
        let (code, ct, body) = call(&r, "GET", "/", "", OWNER);
        assert_eq!(code, StatusCode::OK);
        assert!(ct.starts_with("text/html"), "{ct}");
        assert!(body.contains("<title>Awake</title>"));
        assert_eq!(call(&r, "GET", "/", "", STRANGER).0, StatusCode::FORBIDDEN, "different login");
        assert_eq!(call(&r, "GET", "/api/status", "", UNKNOWN).0, StatusCode::FORBIDDEN, "peer not found");
        assert_eq!(call(&r, "POST", "/api/session/start", r#"{"minutes":null}"#, STRANGER).0, StatusCode::FORBIDDEN);
        assert_eq!(call(&r, "GET", "/api/status", "", Ipv4Addr::LOCALHOST).0, StatusCode::FORBIDDEN, "not a tailnet address");
        assert_eq!(call(&r, "GET", "/nope", "", STRANGER).0, StatusCode::FORBIDDEN, "unknown paths are gated too");
        assert_eq!(call(&r, "GET", "/nope", "", OWNER).0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn self_lookup_failure_is_403_for_everyone() {
        let (app, r) = harness_with(FakeSleep::default(), &[(OWNER.into(), ME)]);
        assert_eq!(call(&r, "POST", "/api/session/start", r#"{"minutes":null}"#, OWNER).0, StatusCode::FORBIDDEN);
        assert!(!app.menu_model(Instant::now()).active);
    }

    #[test]
    fn status_shape_when_idle() {
        let (_, r) = harness(FakeSleep::default());
        let (code, ct, body) = call(&r, "GET", "/api/status", "", OWNER);
        assert_eq!(code, StatusCode::OK);
        assert_eq!(ct, "application/json");
        let v = json(&body);
        assert_eq!(v["session"]["active"], false);
        assert_eq!(v["session"]["ends_at"], serde_json::Value::Null);
        assert_eq!(v["session"]["remaining_seconds"], serde_json::Value::Null);
        assert_eq!(v["session"]["lid_protection"], false);
        assert_eq!(v["helper_connected"], false);
        assert_eq!(v["battery_guard"], true);
        assert_eq!(v["metrics"], serde_json::Value::Null);
    }

    #[test]
    fn start_with_and_without_minutes_returns_status() {
        let (app, r) = harness(FakeSleep::default());
        let (code, _, body) = call(&r, "POST", "/api/session/start", r#"{"minutes":60}"#, OWNER);
        assert_eq!(code, StatusCode::OK);
        let v = json(&body);
        assert_eq!(v["session"]["active"], true);
        assert_eq!(v["session"]["lid_protection"], true);
        let left = v["session"]["remaining_seconds"].as_u64().unwrap();
        assert!((3590..=3600).contains(&left), "{left}");
        assert!(v["session"]["ends_at"].as_str().unwrap().ends_with('Z'));
        let (_, _, body) = call(&r, "POST", "/api/session/start", r#"{"minutes":null}"#, OWNER);
        let v = json(&body);
        assert_eq!(v["session"]["active"], true);
        assert_eq!(v["session"]["remaining_seconds"], serde_json::Value::Null);
        assert_eq!(sleep_calls(&app), vec!["assert", "disable"], "the second start only replaced the timer");
    }

    #[test]
    fn start_rejects_bad_bodies() {
        let (app, r) = harness(FakeSleep::default());
        for body in ["", "not json", r#"{"minutes":0}"#, r#"{"minutes":1441}"#, r#"{"minutes":"x"}"#] {
            let (code, _, text) = call(&r, "POST", "/api/session/start", body, OWNER);
            assert_eq!(code, StatusCode::BAD_REQUEST, "{body:?}");
            assert!(json(&text)["error"].is_string(), "{text}");
        }
        assert!(!app.menu_model(Instant::now()).active);
        assert!(sleep_calls(&app).is_empty());
    }

    #[test]
    fn stop_ends_session_and_is_idempotent() {
        let (app, r) = harness(FakeSleep::default());
        call(&r, "POST", "/api/session/start", r#"{"minutes":null}"#, OWNER);
        let (code, _, body) = call(&r, "POST", "/api/session/stop", "", OWNER);
        assert_eq!(code, StatusCode::OK);
        assert_eq!(json(&body)["session"]["active"], false);
        assert_eq!(sleep_calls(&app), vec!["assert", "disable", "enable", "release"]);
        assert_eq!(call(&r, "POST", "/api/session/stop", "", OWNER).0, StatusCode::OK, "stop while idle");
        assert_eq!(sleep_calls(&app).len(), 4);
    }

    #[test]
    fn screen_off_and_lock_do_not_end_session() {
        let (app, r) = harness(FakeSleep::default());
        call(&r, "POST", "/api/session/start", r#"{"minutes":null}"#, OWNER);
        let (code, ct, body) = call(&r, "POST", "/api/screen/off", "", OWNER);
        assert_eq!((code, ct.as_str(), body.as_str()), (StatusCode::OK, "application/json", r#"{"ok":true}"#));
        assert_eq!(call(&r, "POST", "/api/lock", "", OWNER).2, r#"{"ok":true}"#);
        assert_eq!(*app.power().calls.lock().unwrap(), vec!["screen_off", "lock"]);
        assert!(app.menu_model(Instant::now()).active);
    }

    #[test]
    fn shutdown_needs_confirm_true() {
        let (app, r) = harness(FakeSleep::default());
        call(&r, "POST", "/api/session/start", r#"{"minutes":null}"#, OWNER);
        for body in ["", "{}", r#"{"confirm":false}"#, r#"{"confirm":"yes"}"#] {
            let (code, _, text) = call(&r, "POST", "/api/shutdown", body, OWNER);
            assert_eq!(code, StatusCode::BAD_REQUEST, "{body:?}");
            assert_eq!(json(&text)["error"], "confirm must be true");
        }
        assert!(app.menu_model(Instant::now()).active, "refused requests change nothing");
        let (code, _, body) = call(&r, "POST", "/api/shutdown", r#"{"confirm":true}"#, OWNER);
        assert_eq!((code, body.as_str()), (StatusCode::OK, r#"{"ok":true}"#));
        assert!(!app.menu_model(Instant::now()).active, "shutdown ends the session first");
        assert_eq!(sleep_calls(&app), vec!["assert", "disable", "enable", "release", "shutdown"]);
    }

    #[test]
    fn shutdown_helper_failure_is_503() {
        let (_, r) = harness(FakeSleep { helper_fails: true, ..FakeSleep::default() });
        let (code, _, body) = call(&r, "POST", "/api/shutdown", r#"{"confirm":true}"#, OWNER);
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert!(json(&body)["error"].is_string(), "{body}");
    }
}
