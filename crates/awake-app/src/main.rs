//! Wiring only: logging, config, helper keeper, tick thread, lid watcher, AppKit run loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use awake_app::app::App;
use awake_app::config::{self, Config};
use awake_app::helper::{HelperClient, HelperEvents, PING_INTERVAL, SOCKET_PATH, spawn_keeper};
use awake_app::menu::{self, RealApp};
use awake_app::metrics::{Collector, SAMPLE_INTERVAL};
use awake_app::power::SystemPower;
use awake_app::session::Session;
use awake_app::sleep::RealSleep;
use awake_app::{lid, logging, login_item};
use awake_proto::Request;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

/// Spec 4.5: one `sleep_enable` on the first connection, then forward events to the app.
struct Events {
    app: Arc<RealApp>,
    helper: Arc<HelperClient>,
    reset_done: AtomicBool,
}

impl HelperEvents for Events {
    fn connected(&self) {
        if !self.reset_done.swap(true, Ordering::SeqCst) {
            match self.helper.request_ok(Request::SleepEnable) {
                Ok(()) => tracing::info!("startup sleep reset sent"),
                Err(e) => tracing::warn!("startup sleep reset failed: {e}"),
            }
        }
        self.app.connected();
    }

    fn lost(&self) {
        self.app.lost();
    }
}

/// Once a second: timer and battery guard, lid polling fallback, per-minute menu refresh.
/// Every `SAMPLE_INTERVAL` it also takes the metrics sample, which feeds the battery guard.
fn spawn_tick(app: Arc<RealApp>) {
    std::thread::Builder::new()
        .name("awake-tick".into())
        .spawn(move || {
            // Built on this thread because the HID temperature client is not `Send`.
            let mut collector = Collector::host();
            let sample_every = SAMPLE_INTERVAL.as_secs();
            let mut n: u64 = 0;
            let mut lid_was_closed = false;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                n += 1;
                let metrics = if n.is_multiple_of(sample_every) { Some(collector.sample()) } else { None };
                app.tick(Instant::now(), metrics.as_ref().and_then(|m| m.battery));
                if let Some(m) = metrics {
                    if n == sample_every {
                        tracing::info!("first metrics sample: {m:?}");
                    }
                    app.set_metrics(m);
                }
                if !app.lid_registered()
                    && let Some(c) = lid::read_clamshell()
                {
                    if c.closed && !lid_was_closed {
                        app.lid_changed(true, c.causes_sleep);
                    }
                    lid_was_closed = c.closed;
                }
                if n.is_multiple_of(60) {
                    menu::post_refresh();
                }
            }
        })
        .expect("spawn tick thread");
}

fn main() {
    logging::init(&config::log_dir());
    tracing::info!("awake-app {} starting, pid {}", env!("CARGO_PKG_VERSION"), std::process::id());
    let mtm = MainThreadMarker::new().expect("main runs on the main thread");

    let config_path = Config::default_path();
    let config = Config::load(&config_path);
    let helper = Arc::new(HelperClient::new(SOCKET_PATH));
    let session = Session::new(RealSleep::new(helper.clone()), config.battery_guard);
    let app: Arc<RealApp> = App::new(session, SystemPower, config.clone(), config_path, menu::post_refresh);

    if config.launch_at_login && !login_item::is_enabled() {
        match login_item::set_enabled(true) {
            Ok(()) => tracing::info!("launch at login: {}", login_item::status_text()),
            Err(e) => tracing::warn!("launch at login not registered: {e}"),
        }
    }

    let events = Events { app: app.clone(), helper: helper.clone(), reset_done: AtomicBool::new(false) };
    spawn_keeper(helper, Arc::new(events), PING_INTERVAL);
    spawn_tick(app.clone());

    let nsapp = NSApplication::sharedApplication(mtm);
    nsapp.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    menu::install(mtm, app.clone());

    let lid_app = app.clone();
    // Held until the run loop exits so the registration lives for the whole app.
    let _lid_watcher = match lid::watch(Box::new(move |c| lid_app.lid_changed(c.closed, c.causes_sleep))) {
        Ok(watcher) => {
            app.set_lid_registered(true);
            Some(watcher)
        }
        Err(e) => {
            tracing::warn!("lid notification unavailable ({e}); polling once a second instead");
            None
        }
    };

    nsapp.run();
}
