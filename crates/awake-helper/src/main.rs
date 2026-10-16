use std::path::PathBuf;
use std::sync::Arc;

use awake_helper::commands::{SystemRunner, sleep_enable};
use awake_helper::handler::Handler;
use awake_helper::server::{bind, serve};

const DEFAULT_SOCKET: &str = "/var/run/awake-helper.sock";

fn main() {
    let allowed_uid: u32 = match std::env::var("AWAKE_ALLOWED_UID").ok().and_then(|s| s.trim().parse().ok()) {
        Some(u) => u,
        None => {
            eprintln!("awake-helper: AWAKE_ALLOWED_UID must be set to a numeric uid (check the launchd plist)");
            std::process::exit(2);
        }
    };
    // pmset prints "must be run as root" yet exits 0 when unprivileged, so a
    // non-root helper would answer ok to commands that changed nothing.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("awake-helper: must run as root; pmset silently does nothing otherwise");
        std::process::exit(3);
    }
    let socket = std::env::var("AWAKE_SOCKET").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(DEFAULT_SOCKET));

    let runner = SystemRunner;
    match sleep_enable(&runner) {
        Ok(()) => eprintln!("awake-helper: startup reset, sleep enabled"),
        Err(e) => eprintln!("awake-helper: startup reset failed: {e}"),
    }

    let listener = match bind(&socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("awake-helper: cannot bind {}: {e}", socket.display());
            std::process::exit(1);
        }
    };
    eprintln!("awake-helper: listening on {} for uid {allowed_uid}", socket.display());

    let handler = Arc::new(Handler::new(runner));
    serve(listener, allowed_uid, handler)
}
