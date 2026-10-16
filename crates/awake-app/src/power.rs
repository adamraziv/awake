//! Local actions that need no root: screen off and lock. Spec section 5.

use std::ffi::{CStr, c_void};
use std::io;
use std::process::Command;
use std::sync::Mutex;

pub const PMSET: &str = "/usr/bin/pmset";
const LOGIN_FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/login.framework/login";
const LOCK_SYMBOL: &CStr = c"SACLockScreenImmediate";

pub trait Power: Send + Sync {
    /// Turn the display off now. Does not end the session.
    fn screen_off(&self) -> io::Result<()>;
    /// Lock the screen now. Does not end the session.
    fn lock(&self) -> io::Result<()>;
}

#[derive(Default)]
pub struct SystemPower;

impl Power for SystemPower {
    fn screen_off(&self) -> io::Result<()> {
        run(PMSET, &["displaysleepnow"])
    }

    fn lock(&self) -> io::Result<()> {
        match lock_via_login_framework() {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("SACLockScreenImmediate unavailable ({e}); turning the display off instead");
                self.screen_off()
            }
        }
    }
}

fn run(program: &str, args: &[&str]) -> io::Result<()> {
    let status = Command::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{program} exited with {status}")))
    }
}

pub type LockFn = unsafe extern "C" fn() -> i32;

/// Resolve the private lock call at runtime. The dlopen handle is kept for the process lifetime.
pub fn resolve_lock_fn() -> io::Result<LockFn> {
    // SAFETY: both strings are NUL-terminated; a null result is checked before use; the
    // symbol is a plain `int (*)(void)`.
    unsafe {
        let handle = libc::dlopen(LOGIN_FRAMEWORK.as_ptr(), libc::RTLD_LAZY);
        if handle.is_null() {
            return Err(io::Error::other("login framework not found"));
        }
        let sym = libc::dlsym(handle, LOCK_SYMBOL.as_ptr());
        if sym.is_null() {
            return Err(io::Error::other("SACLockScreenImmediate not found"));
        }
        Ok(std::mem::transmute::<*mut c_void, LockFn>(sym))
    }
}

fn lock_via_login_framework() -> io::Result<()> {
    let lock = resolve_lock_fn()?;
    // SAFETY: `lock` is SACLockScreenImmediate, which takes no arguments.
    let rc = unsafe { lock() };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!("SACLockScreenImmediate returned {rc}")))
    }
}

/// Records calls instead of touching the display. Used by `app.rs` tests.
#[derive(Default)]
pub struct FakePower {
    pub calls: Mutex<Vec<&'static str>>,
}

impl Power for FakePower {
    fn screen_off(&self) -> io::Result<()> {
        self.calls.lock().unwrap().push("screen_off");
        Ok(())
    }

    fn lock(&self) -> io::Result<()> {
        self.calls.lock().unwrap().push("lock");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_symbol_resolves_from_login_framework() {
        resolve_lock_fn().expect("SACLockScreenImmediate should resolve on macOS 26");
    }

    #[test]
    fn fake_power_records_calls() {
        let p = FakePower::default();
        p.screen_off().unwrap();
        p.lock().unwrap();
        assert_eq!(*p.calls.lock().unwrap(), vec!["screen_off", "lock"]);
    }

    #[test]
    fn run_reports_nonzero_exit() {
        assert!(run("/usr/bin/false", &[]).is_err());
        assert!(run("/usr/bin/true", &[]).is_ok());
    }
}
