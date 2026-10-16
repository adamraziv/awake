//! Persisted preferences at ~/Library/Application Support/awake/config.json. Spec section 4.4.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use crate::tailscale::DEFAULT_CLI as DEFAULT_TAILSCALE_CLI;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub battery_guard: bool,
    pub launch_at_login: bool,
    /// Spec 6.2: the `tailscale` binary used for `whois --json`.
    pub tailscale_cli: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self { battery_guard: true, launch_at_login: true, tailscale_cli: PathBuf::from(DEFAULT_TAILSCALE_CLI) }
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        support_dir().join("config.json")
    }

    /// A missing or unreadable file yields the defaults. A corrupt file yields the defaults
    /// too, with a warning, so a bad edit never stops the app from starting.
    pub fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("config at {} is invalid ({e}); using defaults", path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(path, text)
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn support_dir() -> PathBuf {
    home_dir().join("Library/Application Support/awake")
}

pub fn log_dir() -> PathBuf {
    home_dir().join("Library/Logs/awake")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("awake-config-{}-{nanos}", std::process::id())).join(name)
    }

    #[test]
    fn defaults_are_on() {
        let c = Config::default();
        assert!(c.battery_guard);
        assert!(c.launch_at_login);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let p = temp_path("missing.json");
        assert_eq!(Config::load(&p), Config::default());
    }

    #[test]
    fn save_then_load_round_trips_and_creates_parent_dir() {
        let p = temp_path("nested/config.json");
        let c = Config { battery_guard: false, launch_at_login: false, ..Config::default() };
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p), c);
    }

    #[test]
    fn corrupt_file_loads_defaults() {
        let p = temp_path("bad.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(Config::load(&p), Config::default());
    }

    #[test]
    fn missing_and_unknown_fields_are_tolerated() {
        let p = temp_path("partial.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{ "battery_guard": false, "future_field": 1 }"#).unwrap();
        let c = Config::load(&p);
        assert!(!c.battery_guard);
        assert!(c.launch_at_login);
    }

    #[test]
    fn standard_paths_are_under_home() {
        let home = home_dir();
        assert!(Config::default_path().starts_with(&home));
        assert!(support_dir().ends_with("Library/Application Support/awake"));
        assert!(log_dir().ends_with("Library/Logs/awake"));
    }

    #[test]
    fn tailscale_cli_defaults_to_app_bundle_and_round_trips() {
        assert_eq!(Config::default().tailscale_cli, PathBuf::from(DEFAULT_TAILSCALE_CLI));
        let p = temp_path("cli.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{ "tailscale_cli": "/opt/homebrew/bin/tailscale" }"#).unwrap();
        assert_eq!(Config::load(&p).tailscale_cli, PathBuf::from("/opt/homebrew/bin/tailscale"));
    }
}
