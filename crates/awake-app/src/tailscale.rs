//! Tailscale: finding our tailnet address with `getifaddrs`, and resolving who owns a peer IP by
//! running the Tailscale app's CLI. Spec sections 6.1 and 6.2.

use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The CLI inside the standalone Tailscale app. The App Store build installs elsewhere, hence the
/// `tailscale_cli` setting in `config.json`.
pub const DEFAULT_CLI: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
/// `whois` answers in about 25 ms on this Mac; anything slower than this is treated as a failure.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(3);

/// 100.64.0.0/10, the CGNAT range Tailscale assigns from.
pub fn is_tailscale_ip(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

/// The first IPv4 address in 100.64.0.0/10 on any interface. `None` when Tailscale is down or absent.
pub fn detect_ip() -> Option<Ipv4Addr> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list that freeifaddrs below releases once.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return None;
    }
    let mut found = None;
    let mut cur = list;
    while !cur.is_null() {
        // SAFETY: `cur` is a live node of the list; `ifa_addr` may be null and is checked.
        let node = unsafe { &*cur };
        if !node.ifa_addr.is_null() {
            // SAFETY: `ifa_addr` points at a sockaddr; the family is read before any wider cast.
            let family = unsafe { (*node.ifa_addr).sa_family };
            if libc::c_int::from(family) == libc::AF_INET {
                // SAFETY: AF_INET addresses are stored as sockaddr_in.
                let sin = unsafe { &*(node.ifa_addr as *const libc::sockaddr_in) };
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if is_tailscale_ip(ip) {
                    found = Some(ip);
                    break;
                }
            }
        }
        cur = node.ifa_next;
    }
    // SAFETY: `list` came from getifaddrs and nothing above still points into it.
    unsafe { libc::freeifaddrs(list) };
    found
}

/// Runs a command and returns its stdout on exit 0. `None` on spawn failure, non-zero exit, or
/// timeout; a timed-out child is killed. Polled with `try_wait`, so no extra thread is needed; the
/// output is read after exit, which is fine for `whois` (a few KB) but would deadlock past the
/// 64 KB pipe buffer, so do not use this for `status --json` on a large tailnet.
pub fn run_with_timeout(program: &Path, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = Vec::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_end(&mut out);
                }
                return status.success().then_some(out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct Whois {
    #[serde(rename = "UserProfile")]
    user_profile: Option<UserProfile>,
}

#[derive(serde::Deserialize)]
struct UserProfile {
    #[serde(rename = "LoginName")]
    login_name: Option<String>,
}

/// `UserProfile.LoginName` from `tailscale whois --json` output. Empty names count as missing.
pub fn login_name(json: &[u8]) -> Option<String> {
    let whois: Whois = serde_json::from_slice(json).ok()?;
    whois.user_profile?.login_name.filter(|s| !s.is_empty())
}

/// Who owns a tailnet IP. `None` for any failure; the caller turns that into 403 (spec 6.2).
pub trait Identity: Send + Sync + 'static {
    fn login_for(&self, ip: IpAddr) -> Option<String>;
}

/// The real resolver: `<cli> whois --json <ip>`.
pub struct CliIdentity {
    cli: PathBuf,
}

impl CliIdentity {
    pub fn new(cli: impl Into<PathBuf>) -> Self {
        Self { cli: cli.into() }
    }
}

impl Identity for CliIdentity {
    fn login_for(&self, ip: IpAddr) -> Option<String> {
        let out = run_with_timeout(&self.cli, &["whois", "--json", &ip.to_string()], CLI_TIMEOUT);
        match out {
            Some(bytes) => {
                let login = login_name(&bytes);
                if login.is_none() {
                    tracing::warn!("tailscale whois {ip}: no LoginName in output");
                }
                login
            }
            None => {
                tracing::warn!("tailscale whois {ip} failed ({})", self.cli.display());
                None
            }
        }
    }
}

/// Answers from a table and counts lookups. Used by `remote` tests.
#[derive(Default)]
pub struct FakeIdentity {
    pub logins: Mutex<HashMap<IpAddr, String>>,
    pub calls: AtomicUsize,
}

impl FakeIdentity {
    pub fn with(entries: &[(IpAddr, &str)]) -> Self {
        let logins = entries.iter().map(|(ip, login)| (*ip, (*login).to_string())).collect();
        Self { logins: Mutex::new(logins), calls: AtomicUsize::new(0) }
    }
}

impl Identity for FakeIdentity {
    fn login_for(&self, ip: IpAddr) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.logins.lock().unwrap().get(&ip).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape of a real `Tailscale whois --json` reply, trimmed, with identifiers replaced.
    const WHOIS: &str = r#"{
      "Node": { "ID": 1001, "Name": "mac.example-tailnet.ts.net.", "User": 2002,
                "Addresses": ["100.64.0.1/32", "fd7a:115c:a1e0::1/128"], "ComputedName": "mac" },
      "UserProfile": { "ID": 2002, "LoginName": "someone@example.com", "DisplayName": "Someone" },
      "CapMap": null
    }"#;

    #[test]
    fn cgnat_range_bounds() {
        assert!(is_tailscale_ip(Ipv4Addr::new(100, 64, 0, 0)));
        assert!(is_tailscale_ip(Ipv4Addr::new(100, 69, 181, 37)));
        assert!(is_tailscale_ip(Ipv4Addr::new(100, 127, 255, 255)));
        assert!(!is_tailscale_ip(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_tailscale_ip(Ipv4Addr::new(100, 128, 0, 0)));
        assert!(!is_tailscale_ip(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!is_tailscale_ip(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_tailscale_ip(Ipv4Addr::new(192, 168, 1, 198)));
    }

    #[test]
    fn detect_ip_is_in_range_when_present() {
        // Tailscale may be down on the test host; only the range is asserted.
        if let Some(ip) = detect_ip() {
            assert!(is_tailscale_ip(ip), "{ip}");
        }
    }

    #[test]
    fn login_name_parses_whois_output() {
        assert_eq!(login_name(WHOIS.as_bytes()).as_deref(), Some("someone@example.com"));
        assert_eq!(login_name(br#"{"Node":{}}"#), None);
        assert_eq!(login_name(br#"{"UserProfile":{"LoginName":""}}"#), None);
        assert_eq!(login_name(b"peer not found"), None);
        assert_eq!(login_name(b""), None);
    }

    #[test]
    fn run_with_timeout_returns_stdout_on_success_only() {
        assert_eq!(run_with_timeout(Path::new("/bin/echo"), &["hi"], CLI_TIMEOUT).as_deref(), Some(&b"hi\n"[..]));
        assert_eq!(run_with_timeout(Path::new("/usr/bin/false"), &[], CLI_TIMEOUT), None);
        assert_eq!(run_with_timeout(Path::new("/nonexistent/binary"), &[], CLI_TIMEOUT), None);
    }

    #[test]
    fn run_with_timeout_kills_slow_commands() {
        let started = Instant::now();
        assert_eq!(run_with_timeout(Path::new("/bin/sleep"), &["5"], Duration::from_millis(200)), None);
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
    }

    #[test]
    fn cli_identity_on_host() {
        // Runs the real CLI only when Tailscale is installed and up on this Mac.
        let (Some(ip), true) = (detect_ip(), Path::new(DEFAULT_CLI).exists()) else { return };
        let id = CliIdentity::new(DEFAULT_CLI);
        let me = id.login_for(IpAddr::V4(ip)).expect("whois of our own address");
        assert!(me.contains('@'), "{me}");
        // 192.0.2.1 is TEST-NET-1 and never on a tailnet: `peer not found`, exit 1.
        assert_eq!(id.login_for(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))), None);
    }

    #[test]
    fn fake_identity_counts_calls() {
        let a: IpAddr = Ipv4Addr::new(100, 64, 0, 2).into();
        let b: IpAddr = Ipv4Addr::new(100, 64, 0, 3).into();
        let fake = FakeIdentity::with(&[(a, "me@example.com")]);
        assert_eq!(fake.login_for(a).as_deref(), Some("me@example.com"));
        assert_eq!(fake.login_for(b), None);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    }
}
