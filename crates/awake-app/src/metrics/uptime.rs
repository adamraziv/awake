//! Time since boot from `kern.boottime`. Spec section 7.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::sysctl;

pub fn read() -> Option<Duration> {
    let boot: libc::timeval = sysctl::by_name(c"kern.boottime")?;
    let secs = u64::try_from(boot.tv_sec).ok()?;
    let nanos = u32::try_from(boot.tv_usec).ok()?.checked_mul(1000)?;
    SystemTime::now().duration_since(UNIX_EPOCH).ok()?.checked_sub(Duration::new(secs, nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_is_plausible() {
        let up = read().expect("kern.boottime");
        // Longer than it takes to reach a shell, shorter than a year.
        assert!(up > Duration::from_secs(10), "{up:?}");
        assert!(up < Duration::from_secs(365 * 24 * 3600), "{up:?}");
    }
}
