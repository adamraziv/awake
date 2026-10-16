//! Remote control over HTTP on the Tailscale IP. Spec section 6. `auth.rs` decides who may call,
//! `status.rs` shapes `/api/status`, `server.rs` runs the server; the router lives here (Task 6).

pub mod auth;
pub mod status;

/// The whole page. Editing `web/index.html` needs a rebuild; nothing reads it at run time.
pub const INDEX_HTML: &str = include_str!("../../web/index.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_is_embedded_and_self_contained() {
        assert!(INDEX_HTML.contains("<title>Awake</title>"));
        assert!(INDEX_HTML.contains("prefers-color-scheme"));
        assert!(INDEX_HTML.contains("viewport"));
        assert!(!INDEX_HTML.contains("<script src="), "no external scripts");
        assert!(!INDEX_HTML.contains("<link "), "no external stylesheets");
        for path in ["/api/status", "/api/session/start", "/api/session/stop", "/api/screen/off", "/api/lock", "/api/shutdown"] {
            assert!(INDEX_HTML.contains(path), "{path}");
        }
        for minutes in crate::session::PRESET_MINUTES {
            assert!(INDEX_HTML.contains(&format!("data-minutes=\"{minutes}\"")), "preset {minutes}");
        }
    }
}
