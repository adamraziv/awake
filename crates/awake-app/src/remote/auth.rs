//! Per-request authorization: the peer's tailnet login must equal this node's own. Spec 6.2.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::tailscale::{Identity, is_tailscale_ip};

pub const PEER_TTL: Duration = Duration::from_secs(60);
pub const SELF_TTL: Duration = Duration::from_secs(300);
/// A failed lookup is retried sooner so a transient CLI error does not lock the owner out for a minute.
pub const FAILURE_TTL: Duration = Duration::from_secs(5);

struct Entry {
    login: Option<String>,
    expires: Instant,
}

/// One per server instance; the server passes its bound address as `self_ip`.
pub struct Auth {
    identity: Box<dyn Identity>,
    self_ip: Ipv4Addr,
    peers: Mutex<HashMap<IpAddr, Entry>>,
    me: Mutex<Option<Entry>>,
}

impl Auth {
    pub fn new(identity: Box<dyn Identity>, self_ip: Ipv4Addr) -> Self {
        Self { identity, self_ip, peers: Mutex::new(HashMap::new()), me: Mutex::new(None) }
    }

    /// True only when the peer is a tailnet address, both lookups succeed, and the logins match.
    /// Each lookup can take up to `tailscale::CLI_TIMEOUT`; the caller keeps this off the runtime thread.
    pub fn allows(&self, peer: IpAddr, now: Instant) -> bool {
        let IpAddr::V4(v4) = peer else { return false };
        if !is_tailscale_ip(v4) {
            return false;
        }
        let Some(me) = self.self_login(now) else { return false };
        self.peer_login(peer, now).is_some_and(|login| login == me)
    }

    fn self_login(&self, now: Instant) -> Option<String> {
        let mut slot = self.me.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = slot.as_ref()
            && e.expires > now
        {
            return e.login.clone();
        }
        let login = self.identity.login_for(IpAddr::V4(self.self_ip));
        let ttl = if login.is_some() { SELF_TTL } else { FAILURE_TTL };
        *slot = Some(Entry { login: login.clone(), expires: now + ttl });
        login
    }

    /// The mutex is held across the CLI call on purpose: concurrent requests for the same IP then
    /// share one lookup instead of racing the CLI.
    fn peer_login(&self, peer: IpAddr, now: Instant) -> Option<String> {
        let mut peers = self.peers.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = peers.get(&peer)
            && e.expires > now
        {
            return e.login.clone();
        }
        let login = self.identity.login_for(peer);
        let ttl = if login.is_some() { PEER_TTL } else { FAILURE_TTL };
        peers.retain(|_, e| e.expires > now);
        peers.insert(peer, Entry { login: login.clone(), expires: now + ttl });
        login
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tailscale::FakeIdentity;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    const ME: &str = "me@example.com";
    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(100, 64, 0, last))
    }
    fn self_ip() -> Ipv4Addr {
        Ipv4Addr::new(100, 64, 0, 1)
    }

    /// `Auth` owns a `Box<dyn Identity>`, so the fake is shared through an `Arc` wrapper.
    struct Shared(Arc<FakeIdentity>);
    impl Identity for Shared {
        fn login_for(&self, ip: IpAddr) -> Option<String> {
            self.0.login_for(ip)
        }
    }

    fn auth(entries: &[(IpAddr, &str)]) -> (Auth, Arc<FakeIdentity>) {
        let fake = Arc::new(FakeIdentity::with(entries));
        (Auth::new(Box::new(Shared(fake.clone())), self_ip()), fake)
    }

    fn calls(fake: &FakeIdentity) -> usize {
        fake.calls.load(Ordering::SeqCst)
    }

    #[test]
    fn allows_matching_login() {
        let (a, fake) = auth(&[(ip(1), ME), (ip(2), ME)]);
        assert!(a.allows(ip(2), Instant::now()));
        assert_eq!(calls(&fake), 2, "self and peer, one lookup each");
    }

    #[test]
    fn rejects_mismatch_unknown_and_failed_self() {
        let (a, _) = auth(&[(ip(1), ME), (ip(3), "other@example.com")]);
        assert!(!a.allows(ip(3), Instant::now()), "different login");
        assert!(!a.allows(ip(4), Instant::now()), "peer not found");
        let (no_self, fake) = auth(&[(ip(2), ME)]);
        assert!(!no_self.allows(ip(2), Instant::now()), "self lookup failed");
        assert_eq!(calls(&fake), 1, "peer is not looked up when self fails");
    }

    #[test]
    fn rejects_non_tailnet_peers_without_lookup() {
        let (a, fake) = auth(&[(ip(1), ME)]);
        assert!(!a.allows(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)), Instant::now()));
        assert!(!a.allows(IpAddr::V4(Ipv4Addr::LOCALHOST), Instant::now()));
        assert!(!a.allows("fd7a:115c:a1e0::1".parse().unwrap(), Instant::now()));
        assert_eq!(calls(&fake), 0);
    }

    #[test]
    fn caches_peer_for_a_minute_and_self_for_five() {
        let (a, fake) = auth(&[(ip(1), ME), (ip(2), ME)]);
        let t0 = Instant::now();
        assert!(a.allows(ip(2), t0));
        assert_eq!(calls(&fake), 2);
        assert!(a.allows(ip(2), t0 + Duration::from_secs(30)));
        assert_eq!(calls(&fake), 2, "both cached");
        assert!(a.allows(ip(2), t0 + Duration::from_secs(61)));
        assert_eq!(calls(&fake), 3, "peer expired, self still cached");
        assert!(a.allows(ip(2), t0 + Duration::from_secs(301)));
        assert_eq!(calls(&fake), 5, "self expired; peer entry from t0+61 expired at t0+121");
    }

    #[test]
    fn failed_peer_lookup_is_retried_after_five_seconds() {
        let (a, fake) = auth(&[(ip(1), ME)]);
        let t0 = Instant::now();
        assert!(!a.allows(ip(2), t0));
        assert_eq!(calls(&fake), 2);
        assert!(!a.allows(ip(2), t0 + Duration::from_secs(2)));
        assert_eq!(calls(&fake), 2, "failure cached");
        fake.logins.lock().unwrap().insert(ip(2), ME.to_string());
        assert!(a.allows(ip(2), t0 + Duration::from_secs(6)));
        assert_eq!(calls(&fake), 3);
    }

    #[test]
    fn login_change_is_picked_up_after_expiry() {
        let (a, fake) = auth(&[(ip(1), ME), (ip(2), ME)]);
        let t0 = Instant::now();
        assert!(a.allows(ip(2), t0));
        fake.logins.lock().unwrap().insert(ip(2), "other@example.com".to_string());
        assert!(a.allows(ip(2), t0 + Duration::from_secs(59)), "still cached");
        assert!(!a.allows(ip(2), t0 + Duration::from_secs(61)));
    }
}
