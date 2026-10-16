//! File logging under ~/Library/Logs/awake/ as awake.YYYY-MM-DD.log, 7 daily files kept. Spec 8.

use std::path::Path;

use tracing_appender::rolling::{RollingFileAppender, Rotation};

/// Returns false (and prints to stderr) when the log cannot be set up; the app still runs.
pub fn init(dir: &Path) -> bool {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("awake: cannot create log directory {}: {e}", dir.display());
        return false;
    }
    let appender = match RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("awake")
        .filename_suffix("log")
        .max_log_files(7)
        .build(dir)
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("awake: cannot open log in {}: {e}", dir.display());
            return false;
        }
    };
    tracing_subscriber::fmt()
        .with_writer(appender)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_a_dated_log_file() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("awake-log-{}-{nanos}", std::process::id()));
        assert!(init(&dir), "first init in this test binary should install the subscriber");
        tracing::info!("hello from the logging test");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n.starts_with("awake.") && n.ends_with(".log")), "got {names:?}");
    }
}
