//! Resets sleep when the helper is told to stop. Spec section 3.2.
//!
//! launchd sends SIGTERM on shutdown and on `launchctl bootout`. Without this the helper dies
//! with `disablesleep` still on, and if it never starts again nothing turns it off.

use std::io;
use std::thread::JoinHandle;

use crate::commands::{CommandRunner, sleep_enable};

pub const TERMINATION_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

/// Blocks the termination signals on the calling thread, so every thread spawned after this
/// inherits the mask, then waits for one on a dedicated thread. On a signal it runs
/// `sleep_enable` and exits 0. Call before spawning any other thread.
///
/// `sigwait` on a thread keeps the reset out of an async signal handler, where running a
/// process is not allowed. Child processes do not inherit the mask: std resets it on spawn.
pub fn reset_on_termination<R: CommandRunner + 'static>(runner: R) -> io::Result<JoinHandle<()>> {
    let set = termination_set();
    // SAFETY: `set` is initialised and a null old-set pointer is allowed.
    let rc = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    std::thread::Builder::new().name("awake-helper-signals".into()).spawn(move || {
        let sig = wait_for_signal(&set);
        match sleep_enable(&runner) {
            Ok(()) => eprintln!("awake-helper: stopped by signal {sig}, sleep enabled"),
            Err(e) => eprintln!("awake-helper: stopped by signal {sig}, sleep reset failed: {e}"),
        }
        std::process::exit(0);
    })
}

fn termination_set() -> libc::sigset_t {
    // SAFETY: sigemptyset initialises the zeroed set before sigaddset touches it.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in TERMINATION_SIGNALS {
            libc::sigaddset(&mut set, sig);
        }
        set
    }
}

fn wait_for_signal(set: &libc::sigset_t) -> libc::c_int {
    loop {
        let mut sig: libc::c_int = 0;
        // SAFETY: `set` is initialised and `sig` is a valid out-pointer.
        if unsafe { libc::sigwait(set, &mut sig) } == 0 {
            return sig;
        }
    }
}
