//! The only module that knows which external programs the helper runs.

use std::io;
use std::process::Command;

pub const PMSET: &str = "/usr/bin/pmset";
pub const SHUTDOWN: &str = "/sbin/shutdown";

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<()>;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<()> {
        let status = Command::new(program).args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("{program} exited with {status}")))
        }
    }
}

pub fn sleep_disable(r: &dyn CommandRunner) -> io::Result<()> {
    r.run(PMSET, &["-a", "disablesleep", "1"])
}

pub fn sleep_enable(r: &dyn CommandRunner) -> io::Result<()> {
    r.run(PMSET, &["-a", "disablesleep", "0"])
}

/// Resets sleep, then halts. The halt proceeds even if the reset fails,
/// because a machine that is shutting down does not need the setting.
pub fn shutdown(r: &dyn CommandRunner) -> io::Result<()> {
    if let Err(e) = sleep_enable(r) {
        eprintln!("awake-helper: sleep_enable before shutdown failed: {e}");
    }
    r.run(SHUTDOWN, &["-h", "now"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        fail_on: Mutex<Option<String>>,
    }

    impl CommandRunner for Recorder {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push((program.to_string(), args.iter().map(|s| s.to_string()).collect()));
            if self.fail_on.lock().unwrap().as_deref() == Some(program) {
                return Err(std::io::Error::other("boom"));
            }
            Ok(())
        }
    }

    fn calls(r: &Recorder) -> Vec<(String, Vec<String>)> {
        r.calls.lock().unwrap().clone()
    }

    #[test]
    fn sleep_disable_runs_pmset_one() {
        let r = Recorder::default();
        sleep_disable(&r).unwrap();
        assert_eq!(calls(&r), vec![(PMSET.to_string(), vec!["-a".into(), "disablesleep".into(), "1".into()])]);
    }

    #[test]
    fn sleep_enable_runs_pmset_zero() {
        let r = Recorder::default();
        sleep_enable(&r).unwrap();
        assert_eq!(calls(&r), vec![(PMSET.to_string(), vec!["-a".into(), "disablesleep".into(), "0".into()])]);
    }

    #[test]
    fn shutdown_resets_sleep_then_halts() {
        let r = Recorder::default();
        shutdown(&r).unwrap();
        assert_eq!(
            calls(&r),
            vec![
                (PMSET.to_string(), vec!["-a".into(), "disablesleep".into(), "0".into()]),
                (SHUTDOWN.to_string(), vec!["-h".into(), "now".into()]),
            ]
        );
    }

    #[test]
    fn shutdown_still_halts_if_sleep_reset_fails() {
        let r = Recorder::default();
        *r.fail_on.lock().unwrap() = Some(PMSET.to_string());
        let result = shutdown(&r);
        assert!(result.is_ok(), "halt must proceed even if pmset fails");
        assert_eq!(calls(&r).len(), 2);
        assert_eq!(calls(&r)[1].0, SHUTDOWN);
    }

    #[test]
    fn system_runner_reports_nonzero_exit_as_error() {
        let r = SystemRunner;
        let err = r.run("/usr/bin/false", &[]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
        assert!(err.to_string().contains("exit"), "got: {err}");
    }

    #[test]
    fn system_runner_succeeds_on_zero_exit() {
        let r = SystemRunner;
        r.run("/usr/bin/true", &[]).unwrap();
    }

    #[test]
    fn system_runner_reports_missing_program() {
        let r = SystemRunner;
        let err = r.run("/nonexistent/program", &[]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
