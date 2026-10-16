//! The controller: one mutex around session, config, helper status, and the current note.
//! Every mutation ends by asking the UI to refresh. Spec sections 4 and 8.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::helper::HelperEvents;
use crate::metrics::Metrics;
use crate::power::Power;
use crate::session::{Battery, EndReason, Session, SleepController};

pub const WARNING_NO_HELPER: &str = "Lid-close protection unavailable, helper not running";

/// Everything the menu needs, computed under the lock and rendered outside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    pub active: bool,
    pub remaining: Option<Duration>,
    pub lid_protection: bool,
    pub helper_connected: bool,
    pub battery_guard: bool,
    pub launch_at_login: bool,
    pub note: Option<&'static str>,
}

impl MenuModel {
    pub fn status_line(&self) -> String {
        match (self.active, self.remaining) {
            (false, _) => "Awake: Idle".to_string(),
            (true, None) => "Active · until stopped".to_string(),
            (true, Some(d)) => format!("Active · {}", format_remaining(d)),
        }
    }

    pub fn warning(&self) -> Option<&'static str> {
        if self.active && !self.lid_protection { Some(WARNING_NO_HELPER) } else { None }
    }

    pub fn helper_line(&self) -> &'static str {
        if self.helper_connected { "Helper: connected" } else { "Helper: not running" }
    }
}

/// Minutes rounded up, so the display reads "1 min left" until the session actually ends.
pub fn format_remaining(d: Duration) -> String {
    let minutes = d.as_secs().div_ceil(60);
    let (h, m) = (minutes / 60, minutes % 60);
    if h > 0 { format!("{h} h {m} min left") } else { format!("{m} min left") }
}

struct Inner<S: SleepController> {
    session: Session<S>,
    config: Config,
    helper_connected: bool,
    note: Option<&'static str>,
    metrics: Option<Metrics>,
}

pub struct App<S: SleepController, P: Power> {
    inner: Mutex<Inner<S>>,
    power: P,
    config_path: PathBuf,
    lid_registered: AtomicBool,
    refresh: Box<dyn Fn() + Send + Sync>,
}

impl<S: SleepController + Send + 'static, P: Power + 'static> App<S, P> {
    pub fn new(
        session: Session<S>,
        power: P,
        config: Config,
        config_path: PathBuf,
        refresh: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner { session, config, helper_connected: false, note: None, metrics: None }),
            power,
            config_path,
            lid_registered: AtomicBool::new(false),
            refresh: Box::new(refresh),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner<S>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn refresh(&self) {
        (self.refresh)();
    }

    /// `None` minutes means "Until I stop it".
    pub fn start_session(&self, minutes: Option<u64>) {
        let now = Instant::now();
        {
            let mut g = self.lock();
            g.note = None;
            g.session.start(now, minutes.map(|m| Duration::from_secs(m * 60)));
        }
        self.refresh();
    }

    pub fn stop_session(&self, reason: EndReason) {
        {
            let mut g = self.lock();
            if g.session.end(reason) {
                g.note = reason.note();
            }
        }
        self.refresh();
    }

    /// Once a second from the tick thread. Battery is `None` on ticks that did not sample it.
    pub fn tick(&self, now: Instant, battery: Option<Battery>) {
        let ended = {
            let mut g = self.lock();
            let ended = g.session.tick(now, battery);
            if let Some(reason) = ended {
                g.note = reason.note();
            }
            ended
        };
        if ended.is_some() {
            self.refresh();
        }
    }

    /// Every `metrics::SAMPLE_INTERVAL` from the tick thread. No refresh: the menu shows none of it.
    pub fn set_metrics(&self, metrics: Metrics) {
        self.lock().metrics = Some(metrics);
    }

    /// The latest sample, for the remote status (phase 4). `None` before the first sample.
    pub fn metrics(&self) -> Option<Metrics> {
        self.lock().metrics.clone()
    }

    /// Spec 4.6. Decides under the lock, runs the command outside it.
    pub fn lid_changed(&self, closed: bool, causes_sleep: bool) {
        let act = closed && self.lock().session.lid_closed(causes_sleep);
        if act {
            tracing::info!("lid closed during session, turning display off");
            if let Err(e) = self.power.screen_off() {
                tracing::warn!("display off on lid close failed: {e}");
            }
        }
    }

    pub fn screen_off(&self) {
        if let Err(e) = self.power.screen_off() {
            tracing::warn!("screen off failed: {e}");
        }
    }

    pub fn lock_screen(&self) {
        if let Err(e) = self.power.lock() {
            tracing::warn!("lock failed: {e}");
        }
    }

    pub fn set_battery_guard(&self, on: bool) {
        {
            let mut g = self.lock();
            g.session.set_battery_guard(on);
            g.config.battery_guard = on;
            self.save_config(&g.config);
        }
        self.refresh();
    }

    /// Stores the preference only. The SMAppService call lives in `login_item.rs`.
    pub fn set_launch_at_login(&self, on: bool) {
        {
            let mut g = self.lock();
            g.config.launch_at_login = on;
            self.save_config(&g.config);
        }
        self.refresh();
    }

    fn save_config(&self, config: &Config) {
        if let Err(e) = config.save(&self.config_path) {
            tracing::warn!("could not save config to {}: {e}", self.config_path.display());
        }
    }

    pub fn menu_model(&self, now: Instant) -> MenuModel {
        let g = self.lock();
        let snap = g.session.snapshot(now);
        MenuModel {
            active: snap.active,
            remaining: snap.remaining,
            lid_protection: snap.lid_protection,
            helper_connected: g.helper_connected,
            battery_guard: snap.battery_guard,
            launch_at_login: g.config.launch_at_login,
            note: g.note,
        }
    }

    /// Called from the Quit item and from applicationWillTerminate.
    pub fn quit(&self) {
        self.stop_session(EndReason::Quit);
    }

    pub fn lid_registered(&self) -> bool {
        self.lid_registered.load(Ordering::Relaxed)
    }

    pub fn set_lid_registered(&self, on: bool) {
        self.lid_registered.store(on, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn with_session<R>(&self, f: impl FnOnce(&mut Session<S>) -> R) -> R {
        f(&mut self.lock().session)
    }
}

impl<S: SleepController + Send + 'static, P: Power + 'static> HelperEvents for App<S, P> {
    fn connected(&self) {
        {
            let mut g = self.lock();
            g.helper_connected = true;
            g.session.on_helper_connected();
        }
        self.refresh();
    }

    fn lost(&self) {
        {
            let mut g = self.lock();
            g.helper_connected = false;
            g.session.on_helper_lost();
        }
        self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::power::FakePower;
    use crate::session::FakeSleep;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type TestApp = App<FakeSleep, FakePower>;

    fn temp_config() -> PathBuf {
        // The macOS wall clock is microsecond-grained, so parallel tests need a counter to stay apart.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("awake-app-{}-{nanos}-{n}", std::process::id())).join("config.json")
    }

    fn make(config: Config) -> (Arc<TestApp>, Arc<AtomicUsize>, PathBuf) {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let counter = refreshes.clone();
        let path = temp_config();
        let session = Session::new(FakeSleep::default(), config.battery_guard);
        let app = App::new(session, FakePower::default(), config, path.clone(), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (app, refreshes, path)
    }

    fn low_battery() -> Option<Battery> {
        Some(Battery { percent: 5, charging: false, on_ac: false })
    }

    #[test]
    fn start_and_stop_update_model_and_refresh() {
        let (app, refreshes, _) = make(Config::default());
        assert!(!app.menu_model(Instant::now()).active);
        app.start_session(None);
        let m = app.menu_model(Instant::now());
        assert!(m.active);
        assert_eq!(m.remaining, None);
        assert!(m.lid_protection);
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        app.stop_session(EndReason::User);
        let m = app.menu_model(Instant::now());
        assert!(!m.active);
        assert_eq!(m.note, None);
        assert_eq!(refreshes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn timer_start_sets_remaining() {
        let (app, _, _) = make(Config::default());
        app.start_session(Some(30));
        let remaining = app.menu_model(Instant::now()).remaining.unwrap();
        assert!(remaining <= Duration::from_secs(1800));
        assert!(remaining > Duration::from_secs(1790));
    }

    #[test]
    fn battery_guard_tick_ends_session_with_note() {
        let (app, refreshes, _) = make(Config::default());
        app.start_session(None);
        app.tick(Instant::now(), low_battery());
        let m = app.menu_model(Instant::now());
        assert!(!m.active);
        assert_eq!(m.note, Some("Session ended: battery guard"));
        assert_eq!(refreshes.load(Ordering::SeqCst), 2);
        // The next start clears the note.
        app.start_session(None);
        assert_eq!(app.menu_model(Instant::now()).note, None);
    }

    #[test]
    fn uneventful_tick_does_not_refresh() {
        let (app, refreshes, _) = make(Config::default());
        app.start_session(None);
        app.tick(Instant::now(), Some(Battery { percent: 80, charging: false, on_ac: false }));
        app.tick(Instant::now(), None);
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn lid_close_turns_screen_off_only_while_active_and_lid_causes_sleep() {
        let (app, _, _) = make(Config::default());
        app.lid_changed(true, true);
        assert!(app.power.calls.lock().unwrap().is_empty());
        app.start_session(None);
        app.lid_changed(true, false);
        app.lid_changed(false, true);
        assert!(app.power.calls.lock().unwrap().is_empty());
        app.lid_changed(true, true);
        app.lid_changed(true, true);
        assert_eq!(*app.power.calls.lock().unwrap(), vec!["screen_off", "screen_off"]);
    }

    #[test]
    fn manual_actions_do_not_end_session() {
        let (app, _, _) = make(Config::default());
        app.start_session(None);
        app.screen_off();
        app.lock_screen();
        assert_eq!(*app.power.calls.lock().unwrap(), vec!["screen_off", "lock"]);
        assert!(app.menu_model(Instant::now()).active);
    }

    #[test]
    fn helper_events_update_model_and_resend_disable() {
        let (app, refreshes, _) = make(Config::default());
        assert!(!app.menu_model(Instant::now()).helper_connected);
        app.connected();
        assert!(app.menu_model(Instant::now()).helper_connected);
        app.start_session(None);
        app.lost();
        let m = app.menu_model(Instant::now());
        assert!(!m.helper_connected);
        assert!(!m.lid_protection);
        assert_eq!(m.warning(), Some(WARNING_NO_HELPER));
        app.connected();
        let m = app.menu_model(Instant::now());
        assert!(m.lid_protection);
        assert_eq!(m.warning(), None);
        let calls = app.with_session(|s| s.sleep().calls.clone());
        assert_eq!(calls, vec!["assert", "disable", "disable"]);
        assert_eq!(refreshes.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn battery_guard_toggle_persists_and_applies() {
        let (app, _, path) = make(Config::default());
        app.set_battery_guard(false);
        assert!(!Config::load(&path).battery_guard);
        assert!(!app.menu_model(Instant::now()).battery_guard);
        app.start_session(None);
        app.tick(Instant::now(), low_battery());
        assert!(app.menu_model(Instant::now()).active);
    }

    #[test]
    fn launch_at_login_pref_persists() {
        let (app, _, path) = make(Config::default());
        app.set_launch_at_login(false);
        assert!(!Config::load(&path).launch_at_login);
        assert!(!app.menu_model(Instant::now()).launch_at_login);
    }

    #[test]
    fn metrics_cache_round_trips_without_refresh() {
        let (app, refreshes, _) = make(Config::default());
        assert_eq!(app.metrics(), None);
        let m = Metrics {
            cpu: None,
            mem: None,
            disk: None,
            battery: Some(Battery { percent: 80, charging: false, on_ac: true }),
            uptime: Some(Duration::from_secs(60)),
            cpu_temp_c: Some(48.2),
            thermal: None,
            sampled_at: std::time::UNIX_EPOCH,
        };
        app.set_metrics(m.clone());
        assert_eq!(app.metrics(), Some(m));
        // Nothing in the menu shows metrics, so storing them never triggers a refresh.
        assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn quit_ends_session_through_end() {
        let (app, _, _) = make(Config::default());
        app.start_session(None);
        app.quit();
        assert!(!app.menu_model(Instant::now()).active);
        let calls = app.with_session(|s| s.sleep().calls.clone());
        assert_eq!(calls, vec!["assert", "disable", "enable", "release"]);
    }

    #[test]
    fn lid_registered_flag() {
        let (app, _, _) = make(Config::default());
        assert!(!app.lid_registered());
        app.set_lid_registered(true);
        assert!(app.lid_registered());
    }

    #[test]
    fn menu_text() {
        let mut m = MenuModel {
            active: false,
            remaining: None,
            lid_protection: false,
            helper_connected: false,
            battery_guard: true,
            launch_at_login: true,
            note: None,
        };
        assert_eq!(m.status_line(), "Awake: Idle");
        assert_eq!(m.helper_line(), "Helper: not running");
        assert_eq!(m.warning(), None);
        m.active = true;
        m.lid_protection = true;
        m.helper_connected = true;
        assert_eq!(m.status_line(), "Active · until stopped");
        assert_eq!(m.helper_line(), "Helper: connected");
        m.remaining = Some(Duration::from_secs(3540));
        assert_eq!(m.status_line(), "Active · 59 min left");
        assert_eq!(format_remaining(Duration::from_secs(7200)), "2 h 0 min left");
        assert_eq!(format_remaining(Duration::from_secs(3660)), "1 h 1 min left");
        assert_eq!(format_remaining(Duration::from_secs(30)), "1 min left");
        assert_eq!(format_remaining(Duration::ZERO), "0 min left");
    }
}
