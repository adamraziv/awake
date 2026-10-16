//! Session state machine. Pure logic behind the SleepController trait. Spec section 4.

use std::time::{Duration, Instant};

pub const BATTERY_GUARD_PERCENT: u8 = 10;
pub const PRESET_MINUTES: [u64; 5] = [30, 60, 120, 240, 480];

/// Side effects a session needs. `sleep.rs` has the real one; `FakeSleep` is for tests.
pub trait SleepController {
    /// Create the PreventUserIdleSystemSleep assertion. Must be idempotent.
    fn assert_idle_sleep(&mut self) -> Result<(), String>;
    /// Release the assertion. Must be idempotent.
    fn release_idle_sleep(&mut self);
    /// Ask the helper for `pmset -a disablesleep 1`. Err when unreachable, busy, or refused.
    fn helper_sleep_disable(&mut self) -> Result<(), String>;
    /// Ask the helper for `pmset -a disablesleep 0`. Failures are logged, never surfaced (spec 4.2).
    fn helper_sleep_enable(&mut self);
    /// Ask the helper to shut the Mac down (spec 5). Err when unreachable or refused.
    fn helper_shutdown(&mut self) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Active { ends_at: Option<Instant> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    User,
    Timer,
    BatteryGuard,
    Remote,
    Shutdown,
    Quit,
}

impl EndReason {
    /// Text the menu shows after an automatic end. User-initiated ends show nothing.
    pub fn note(self) -> Option<&'static str> {
        match self {
            EndReason::BatteryGuard => Some("Session ended: battery guard"),
            EndReason::Timer => Some("Session ended: timer"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
    pub on_ac: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub active: bool,
    pub remaining: Option<Duration>,
    pub lid_protection: bool,
    pub battery_guard: bool,
}

pub struct Session<S: SleepController> {
    sleep: S,
    state: State,
    lid_protection: bool,
    battery_guard: bool,
}

impl<S: SleepController> Session<S> {
    pub fn new(sleep: S, battery_guard: bool) -> Self {
        Self { sleep, state: State::Idle, lid_protection: false, battery_guard }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, State::Active { .. })
    }

    pub fn lid_protection(&self) -> bool {
        self.lid_protection
    }

    pub fn battery_guard(&self) -> bool {
        self.battery_guard
    }

    pub fn set_battery_guard(&mut self, on: bool) {
        self.battery_guard = on;
    }

    pub fn sleep(&self) -> &S {
        &self.sleep
    }

    pub fn sleep_mut(&mut self) -> &mut S {
        &mut self.sleep
    }

    /// Spec 4.1. A start while active only replaces the timer.
    pub fn start(&mut self, now: Instant, duration: Option<Duration>) {
        let ends_at = duration.map(|d| now + d);
        if self.is_active() {
            self.state = State::Active { ends_at };
            tracing::info!("session timer replaced: {:?}", duration);
            return;
        }
        if let Err(e) = self.sleep.assert_idle_sleep() {
            tracing::error!("idle sleep assertion failed: {e}");
        }
        self.lid_protection = match self.sleep.helper_sleep_disable() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("lid-close protection unavailable: {e}");
                false
            }
        };
        self.state = State::Active { ends_at };
        tracing::info!("session started, duration {:?}, lid_protection {}", duration, self.lid_protection);
    }

    /// Spec 4.2. The one and only end path. Returns false when already idle.
    pub fn end(&mut self, reason: EndReason) -> bool {
        if !self.is_active() {
            return false;
        }
        self.sleep.helper_sleep_enable();
        self.sleep.release_idle_sleep();
        self.state = State::Idle;
        self.lid_protection = false;
        tracing::info!("session ended: {reason:?}");
        true
    }

    /// Called about once a second. Ends the session on timer expiry or a battery guard trip.
    pub fn tick(&mut self, now: Instant, battery: Option<Battery>) -> Option<EndReason> {
        let State::Active { ends_at } = self.state else {
            return None;
        };
        if let Some(t) = ends_at
            && now >= t
        {
            self.end(EndReason::Timer);
            return Some(EndReason::Timer);
        }
        if self.battery_guard
            && let Some(b) = battery
            && !b.on_ac
            && b.percent <= BATTERY_GUARD_PERCENT
        {
            self.end(EndReason::BatteryGuard);
            return Some(EndReason::BatteryGuard);
        }
        None
    }

    /// Spec 4.6. True when the caller should run the Screen off action.
    pub fn lid_closed(&self, causes_sleep: bool) -> bool {
        self.is_active() && causes_sleep
    }

    /// Spec 4.5. The helper resets sleep when the connection drops, so re-send on reconnect.
    pub fn on_helper_connected(&mut self) {
        if self.is_active() {
            self.lid_protection = self.sleep.helper_sleep_disable().is_ok();
            tracing::info!("helper reconnected during session, lid_protection {}", self.lid_protection);
        }
    }

    pub fn on_helper_lost(&mut self) {
        if self.is_active() {
            self.lid_protection = false;
        }
    }

    pub fn snapshot(&self, now: Instant) -> Snapshot {
        let remaining = match self.state {
            State::Active { ends_at: Some(t) } => Some(t.saturating_duration_since(now)),
            _ => None,
        };
        Snapshot {
            active: self.is_active(),
            remaining,
            lid_protection: self.lid_protection,
            battery_guard: self.battery_guard,
        }
    }
}

/// Records calls in order. Public so `app.rs` tests can use it too.
#[derive(Default)]
pub struct FakeSleep {
    pub calls: Vec<&'static str>,
    pub helper_fails: bool,
    pub assertion_fails: bool,
}

impl SleepController for FakeSleep {
    fn assert_idle_sleep(&mut self) -> Result<(), String> {
        self.calls.push("assert");
        if self.assertion_fails { Err("fake assertion failure".into()) } else { Ok(()) }
    }

    fn release_idle_sleep(&mut self) {
        self.calls.push("release");
    }

    fn helper_sleep_disable(&mut self) -> Result<(), String> {
        self.calls.push("disable");
        if self.helper_fails { Err("fake helper down".into()) } else { Ok(()) }
    }

    fn helper_sleep_enable(&mut self) {
        self.calls.push("enable");
    }

    fn helper_shutdown(&mut self) -> Result<(), String> {
        self.calls.push("shutdown");
        if self.helper_fails { Err("fake helper down".into()) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn session() -> Session<FakeSleep> {
        Session::new(FakeSleep::default(), true)
    }

    fn calls(s: &Session<FakeSleep>) -> Vec<&'static str> {
        s.sleep().calls.clone()
    }

    #[test]
    fn start_asserts_then_disables_sleep_and_is_active() {
        let mut s = session();
        let now = Instant::now();
        s.start(now, None);
        assert!(s.is_active());
        assert_eq!(s.state(), State::Active { ends_at: None });
        assert!(s.lid_protection());
        assert_eq!(calls(&s), vec!["assert", "disable"]);
    }

    #[test]
    fn start_with_duration_sets_ends_at_and_remaining() {
        let mut s = session();
        let now = Instant::now();
        s.start(now, Some(Duration::from_secs(3600)));
        assert_eq!(s.state(), State::Active { ends_at: Some(now + Duration::from_secs(3600)) });
        let snap = s.snapshot(now + Duration::from_secs(60));
        assert!(snap.active);
        assert_eq!(snap.remaining, Some(Duration::from_secs(3540)));
    }

    #[test]
    fn end_enables_sleep_before_releasing_assertion() {
        let mut s = session();
        s.start(Instant::now(), None);
        assert!(s.end(EndReason::User));
        assert!(!s.is_active());
        assert!(!s.lid_protection());
        assert_eq!(calls(&s), vec!["assert", "disable", "enable", "release"]);
    }

    #[test]
    fn end_while_idle_is_a_noop() {
        let mut s = session();
        assert!(!s.end(EndReason::User));
        assert!(calls(&s).is_empty());
    }

    #[test]
    fn start_while_active_replaces_timer_without_new_side_effects() {
        let mut s = session();
        let now = Instant::now();
        s.start(now, None);
        s.start(now, Some(Duration::from_secs(600)));
        assert_eq!(s.state(), State::Active { ends_at: Some(now + Duration::from_secs(600)) });
        assert_eq!(calls(&s), vec!["assert", "disable"]);
        s.start(now, None);
        assert_eq!(s.state(), State::Active { ends_at: None });
    }

    #[test]
    fn timer_expiry_ends_session_on_tick() {
        let mut s = session();
        let now = Instant::now();
        s.start(now, Some(Duration::from_secs(60)));
        assert_eq!(s.tick(now + Duration::from_secs(59), None), None);
        assert!(s.is_active());
        assert_eq!(s.tick(now + Duration::from_secs(60), None), Some(EndReason::Timer));
        assert!(!s.is_active());
        assert_eq!(calls(&s), vec!["assert", "disable", "enable", "release"]);
    }

    #[test]
    fn tick_while_idle_does_nothing() {
        let mut s = session();
        assert_eq!(s.tick(Instant::now(), Some(Battery { percent: 1, charging: false, on_ac: false })), None);
        assert!(calls(&s).is_empty());
    }

    #[test]
    fn battery_guard_trips_at_threshold_on_battery() {
        let mut s = session();
        let now = Instant::now();
        s.start(now, None);
        let low = Battery { percent: BATTERY_GUARD_PERCENT, charging: false, on_ac: false };
        assert_eq!(s.tick(now, Some(low)), Some(EndReason::BatteryGuard));
        assert!(!s.is_active());
    }

    #[test]
    fn battery_guard_ignores_ac_and_higher_levels() {
        let mut s = session();
        let now = Instant::now();
        s.start(now, None);
        assert_eq!(s.tick(now, Some(Battery { percent: 5, charging: true, on_ac: true })), None);
        assert_eq!(s.tick(now, Some(Battery { percent: 11, charging: false, on_ac: false })), None);
        assert!(s.is_active());
    }

    #[test]
    fn battery_guard_can_be_disabled() {
        let mut s = session();
        s.set_battery_guard(false);
        let now = Instant::now();
        s.start(now, None);
        assert_eq!(s.tick(now, Some(Battery { percent: 1, charging: false, on_ac: false })), None);
        assert!(s.is_active());
        assert!(!s.snapshot(now).battery_guard);
    }

    #[test]
    fn helper_failure_keeps_session_but_drops_lid_protection() {
        let mut s = Session::new(FakeSleep { helper_fails: true, ..Default::default() }, true);
        s.start(Instant::now(), None);
        assert!(s.is_active());
        assert!(!s.lid_protection());
        assert_eq!(calls(&s), vec!["assert", "disable"]);
    }

    #[test]
    fn assertion_failure_still_starts_with_helper() {
        let mut s = Session::new(FakeSleep { assertion_fails: true, ..Default::default() }, true);
        s.start(Instant::now(), None);
        assert!(s.is_active());
        assert!(s.lid_protection());
        assert_eq!(calls(&s), vec!["assert", "disable"]);
    }

    #[test]
    fn lid_closed_triggers_screen_off_only_when_active_and_lid_causes_sleep() {
        let mut s = session();
        assert!(!s.lid_closed(true));
        s.start(Instant::now(), None);
        assert!(s.lid_closed(true));
        assert!(!s.lid_closed(false));
    }

    #[test]
    fn helper_reconnect_resends_disable_while_active() {
        let mut s = Session::new(FakeSleep { helper_fails: true, ..Default::default() }, true);
        s.start(Instant::now(), None);
        assert!(!s.lid_protection());
        s.sleep_mut().helper_fails = false;
        s.on_helper_connected();
        assert!(s.lid_protection());
        assert_eq!(calls(&s), vec!["assert", "disable", "disable"]);
    }

    #[test]
    fn helper_reconnect_while_idle_sends_nothing() {
        let mut s = session();
        s.on_helper_connected();
        assert!(calls(&s).is_empty());
    }

    #[test]
    fn helper_lost_drops_lid_protection_while_active() {
        let mut s = session();
        s.start(Instant::now(), None);
        s.on_helper_lost();
        assert!(s.is_active());
        assert!(!s.lid_protection());
    }

    #[test]
    fn end_reason_notes() {
        assert_eq!(EndReason::BatteryGuard.note(), Some("Session ended: battery guard"));
        assert_eq!(EndReason::Timer.note(), Some("Session ended: timer"));
        assert_eq!(EndReason::User.note(), None);
    }

    #[test]
    fn fake_helper_shutdown_records_and_fails_with_helper() {
        let mut ok = FakeSleep::default();
        assert_eq!(ok.helper_shutdown(), Ok(()));
        assert_eq!(ok.calls, vec!["shutdown"]);
        let mut down = FakeSleep { helper_fails: true, ..FakeSleep::default() };
        assert!(down.helper_shutdown().is_err());
    }
}
