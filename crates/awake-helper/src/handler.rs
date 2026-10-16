//! Pure request logic: owner tracking and dispatch. No sockets here.

use std::sync::Mutex;

use awake_proto::{Request, Response};

use crate::commands::{self, CommandRunner};

pub type ConnId = u64;

pub struct Handler<R: CommandRunner> {
    runner: R,
    owner: Mutex<Option<ConnId>>,
}

impl<R: CommandRunner> Handler<R> {
    pub fn new(runner: R) -> Self {
        Self { runner, owner: Mutex::new(None) }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    pub fn owner(&self) -> Option<ConnId> {
        *self.owner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn handle(&self, conn: ConnId, req: Request) -> Response {
        match req {
            Request::Ping => Response::Ok,
            Request::SleepDisable => self.sleep_disable(conn),
            Request::SleepEnable => self.sleep_enable(conn),
            Request::Shutdown => self.shutdown(),
        }
    }

    pub fn on_disconnect(&self, conn: ConnId) {
        let mut owner = self.owner.lock().unwrap_or_else(|p| p.into_inner());
        if *owner == Some(conn) {
            *owner = None;
            if let Err(e) = commands::sleep_enable(&self.runner) {
                eprintln!("awake-helper: reset on disconnect of conn {conn} failed: {e}");
            } else {
                eprintln!("awake-helper: owner conn {conn} disconnected, sleep re-enabled");
            }
        }
    }

    fn sleep_disable(&self, conn: ConnId) -> Response {
        let mut owner = self.owner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(o) = *owner
            && o != conn
        {
            return Response::Busy;
        }
        match commands::sleep_disable(&self.runner) {
            Ok(()) => {
                *owner = Some(conn);
                Response::Ok
            }
            Err(e) => Response::Err(e.to_string()),
        }
    }

    fn sleep_enable(&self, conn: ConnId) -> Response {
        let mut owner = self.owner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(o) = *owner
            && o != conn
        {
            return Response::Busy;
        }
        *owner = None;
        match commands::sleep_enable(&self.runner) {
            Ok(()) => Response::Ok,
            Err(e) => Response::Err(e.to_string()),
        }
    }

    fn shutdown(&self) -> Response {
        let mut owner = self.owner.lock().unwrap_or_else(|p| p.into_inner());
        *owner = None;
        match commands::shutdown(&self.runner) {
            Ok(()) => Response::Ok,
            Err(e) => Response::Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{PMSET, SHUTDOWN};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        fail: Mutex<bool>,
    }

    impl CommandRunner for Recorder {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<()> {
            self.calls.lock().unwrap().push(format!("{program} {}", args.join(" ")));
            if *self.fail.lock().unwrap() {
                return Err(std::io::Error::other("pmset failed"));
            }
            Ok(())
        }
    }

    fn calls(h: &Handler<Recorder>) -> Vec<String> {
        h.runner().calls.lock().unwrap().clone()
    }

    fn disable_cmd() -> String {
        format!("{PMSET} -a disablesleep 1")
    }
    fn enable_cmd() -> String {
        format!("{PMSET} -a disablesleep 0")
    }

    #[test]
    fn ping_is_ok_and_does_nothing() {
        let h = Handler::new(Recorder::default());
        assert_eq!(h.handle(1, Request::Ping), Response::Ok);
        assert!(calls(&h).is_empty());
        assert_eq!(h.owner(), None);
    }

    #[test]
    fn sleep_disable_takes_ownership() {
        let h = Handler::new(Recorder::default());
        assert_eq!(h.handle(7, Request::SleepDisable), Response::Ok);
        assert_eq!(h.owner(), Some(7));
        assert_eq!(calls(&h), vec![disable_cmd()]);
    }

    #[test]
    fn sleep_disable_from_owner_is_idempotent() {
        let h = Handler::new(Recorder::default());
        h.handle(7, Request::SleepDisable);
        assert_eq!(h.handle(7, Request::SleepDisable), Response::Ok);
        assert_eq!(h.owner(), Some(7));
        assert_eq!(calls(&h), vec![disable_cmd(), disable_cmd()]);
    }

    #[test]
    fn sleep_disable_from_other_connection_is_busy() {
        let h = Handler::new(Recorder::default());
        h.handle(7, Request::SleepDisable);
        assert_eq!(h.handle(8, Request::SleepDisable), Response::Busy);
        assert_eq!(h.owner(), Some(7));
        assert_eq!(calls(&h).len(), 1);
    }

    #[test]
    fn sleep_disable_failure_leaves_no_owner() {
        let h = Handler::new(Recorder::default());
        *h.runner().fail.lock().unwrap() = true;
        assert_eq!(h.handle(7, Request::SleepDisable), Response::Err("pmset failed".into()));
        assert_eq!(h.owner(), None);
    }

    #[test]
    fn sleep_enable_from_owner_releases() {
        let h = Handler::new(Recorder::default());
        h.handle(7, Request::SleepDisable);
        assert_eq!(h.handle(7, Request::SleepEnable), Response::Ok);
        assert_eq!(h.owner(), None);
        assert_eq!(calls(&h), vec![disable_cmd(), enable_cmd()]);
    }

    #[test]
    fn sleep_enable_from_non_owner_is_busy() {
        let h = Handler::new(Recorder::default());
        h.handle(7, Request::SleepDisable);
        assert_eq!(h.handle(8, Request::SleepEnable), Response::Busy);
        assert_eq!(h.owner(), Some(7));
        assert_eq!(calls(&h).len(), 1);
    }

    #[test]
    fn sleep_enable_with_no_owner_runs_anyway() {
        let h = Handler::new(Recorder::default());
        assert_eq!(h.handle(3, Request::SleepEnable), Response::Ok);
        assert_eq!(calls(&h), vec![enable_cmd()]);
    }

    #[test]
    fn disconnect_of_owner_resets_sleep() {
        let h = Handler::new(Recorder::default());
        h.handle(7, Request::SleepDisable);
        h.on_disconnect(7);
        assert_eq!(h.owner(), None);
        assert_eq!(calls(&h), vec![disable_cmd(), enable_cmd()]);
    }

    #[test]
    fn disconnect_of_non_owner_does_nothing() {
        let h = Handler::new(Recorder::default());
        h.handle(7, Request::SleepDisable);
        h.on_disconnect(8);
        assert_eq!(h.owner(), Some(7));
        assert_eq!(calls(&h).len(), 1);
    }

    #[test]
    fn disconnect_with_no_owner_does_nothing() {
        let h = Handler::new(Recorder::default());
        h.on_disconnect(8);
        assert!(calls(&h).is_empty());
    }

    #[test]
    fn shutdown_clears_owner_and_halts() {
        let h = Handler::new(Recorder::default());
        h.handle(7, Request::SleepDisable);
        assert_eq!(h.handle(9, Request::Shutdown), Response::Ok);
        assert_eq!(h.owner(), None);
        assert_eq!(calls(&h), vec![disable_cmd(), enable_cmd(), format!("{SHUTDOWN} -h now")]);
    }

    #[test]
    fn shutdown_failure_is_reported() {
        let h = Handler::new(Recorder::default());
        *h.runner().fail.lock().unwrap() = true;
        assert_eq!(h.handle(9, Request::Shutdown), Response::Err("pmset failed".into()));
    }
}
