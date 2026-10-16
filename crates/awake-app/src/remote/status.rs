//! The `/api/status` document, built from the menu model and the cached metrics. Spec 6.3.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::app::MenuModel;
use crate::metrics::Metrics;
use crate::metrics::mem::MemoryPressure;
use crate::metrics::thermal::ThermalState;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub session: SessionStatus,
    pub helper_connected: bool,
    pub battery_guard: bool,
    /// `None` until the first sample, about five seconds after launch.
    pub metrics: Option<MetricsStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionStatus {
    pub active: bool,
    pub ends_at: Option<String>,
    pub remaining_seconds: Option<u64>,
    pub lid_protection: bool,
}

/// Field names and order are the spec's. Every reader fails on its own, so every field is optional.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricsStatus {
    pub cpu_percent: Option<f32>,
    pub cpu_cores: Option<u32>,
    pub ram_used_bytes: Option<u64>,
    pub ram_total_bytes: Option<u64>,
    pub memory_pressure: Option<&'static str>,
    pub disk_used_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub battery_percent: Option<u8>,
    pub charging: Option<bool>,
    pub on_ac: Option<bool>,
    pub uptime_seconds: Option<u64>,
    pub cpu_temp_c: Option<f32>,
    pub thermal_state: Option<&'static str>,
    pub sampled_at: String,
}

pub fn build(model: &MenuModel, metrics: Option<&Metrics>, now: SystemTime) -> Status {
    Status {
        session: SessionStatus {
            active: model.active,
            ends_at: model.remaining.map(|r| rfc3339(now + r)),
            remaining_seconds: model.remaining.map(|r| r.as_secs()),
            lid_protection: model.lid_protection,
        },
        helper_connected: model.helper_connected,
        battery_guard: model.battery_guard,
        metrics: metrics.map(metrics_status),
    }
}

fn metrics_status(m: &Metrics) -> MetricsStatus {
    MetricsStatus {
        cpu_percent: m.cpu.and_then(|c| c.percent),
        cpu_cores: m.cpu.map(|c| c.cores),
        ram_used_bytes: m.mem.map(|x| x.used_bytes),
        ram_total_bytes: m.mem.map(|x| x.total_bytes),
        memory_pressure: m.mem.and_then(|x| x.pressure).map(MemoryPressure::as_str),
        disk_used_bytes: m.disk.map(|d| d.used_bytes),
        disk_total_bytes: m.disk.map(|d| d.total_bytes),
        battery_percent: m.battery.map(|b| b.percent),
        charging: m.battery.map(|b| b.charging),
        on_ac: m.battery.map(|b| b.on_ac),
        uptime_seconds: m.uptime.map(|u| u.as_secs()),
        cpu_temp_c: m.cpu_temp_c,
        thermal_state: m.thermal.map(ThermalState::as_str),
        sampled_at: rfc3339(m.sampled_at),
    }
}

/// `2026-10-16T12:00:00Z`. Whole seconds, UTC, no dependency on a date crate.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Howard Hinnant's civil_from_days; exact for every date from 1970 on.
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::cpu::Cpu;
    use crate::metrics::disk::Disk;
    use crate::metrics::mem::Mem;
    use crate::session::Battery;

    fn model(active: bool, remaining: Option<Duration>) -> MenuModel {
        MenuModel {
            active,
            remaining,
            lid_protection: active,
            helper_connected: active,
            battery_guard: true,
            launch_at_login: true,
            note: None,
            remote_url: None,
        }
    }

    #[test]
    fn rfc3339_known_instants() {
        let at = |s: u64| rfc3339(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(at(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(at(1_792_152_000), "2026-10-16T12:00:00Z");
        assert_eq!(rfc3339(UNIX_EPOCH + Duration::from_millis(1500)), "1970-01-01T00:00:01Z", "sub-second part dropped");
    }

    #[test]
    fn idle_status_without_metrics() {
        let v = serde_json::to_value(build(&model(false, None), None, UNIX_EPOCH)).unwrap();
        assert_eq!(v["session"]["active"], false);
        assert_eq!(v["session"]["ends_at"], serde_json::Value::Null);
        assert_eq!(v["session"]["remaining_seconds"], serde_json::Value::Null);
        assert_eq!(v["session"]["lid_protection"], false);
        assert_eq!(v["helper_connected"], false);
        assert_eq!(v["battery_guard"], true);
        assert_eq!(v["metrics"], serde_json::Value::Null);
    }

    #[test]
    fn active_status_with_full_metrics_matches_spec_shape() {
        // 59 minutes before the spec's example `ends_at`.
        let now = UNIX_EPOCH + Duration::from_secs(1_792_152_000 - 3540);
        let metrics = Metrics {
            cpu: Some(Cpu { cores: 8, percent: Some(12.5) }),
            mem: Some(Mem { used_bytes: 1234, total_bytes: 5678, pressure: Some(MemoryPressure::Warn) }),
            disk: Some(Disk { used_bytes: 10, total_bytes: 20 }),
            battery: Some(Battery { percent: 87, charging: false, on_ac: true }),
            uptime: Some(Duration::from_secs(12345)),
            cpu_temp_c: Some(48.2),
            thermal: Some(ThermalState::Nominal),
            sampled_at: now,
        };
        let v = serde_json::to_value(build(&model(true, Some(Duration::from_secs(3540))), Some(&metrics), now)).unwrap();
        assert_eq!(v["session"]["active"], true);
        assert_eq!(v["session"]["ends_at"], "2026-10-16T12:00:00Z");
        assert_eq!(v["session"]["remaining_seconds"], 3540);
        assert_eq!(v["session"]["lid_protection"], true);
        assert_eq!(v["helper_connected"], true);
        let m = &v["metrics"];
        assert_eq!(m["cpu_percent"], 12.5);
        assert_eq!(m["cpu_cores"], 8);
        assert_eq!(m["ram_used_bytes"], 1234);
        assert_eq!(m["ram_total_bytes"], 5678);
        assert_eq!(m["memory_pressure"], "warn");
        assert_eq!(m["disk_used_bytes"], 10);
        assert_eq!(m["disk_total_bytes"], 20);
        assert_eq!(m["battery_percent"], 87);
        assert_eq!(m["charging"], false);
        assert_eq!(m["on_ac"], true);
        assert_eq!(m["uptime_seconds"], 12345);
        assert!((m["cpu_temp_c"].as_f64().unwrap() - 48.2).abs() < 0.01);
        assert_eq!(m["thermal_state"], "nominal");
        assert_eq!(m["sampled_at"], "2026-10-16T11:01:00Z");
        assert_eq!(m.as_object().unwrap().len(), 14, "exactly the spec's fields");
    }

    #[test]
    fn missing_readers_become_nulls() {
        let metrics = Metrics {
            cpu: Some(Cpu { cores: 8, percent: None }),
            mem: None,
            disk: None,
            battery: None,
            uptime: None,
            cpu_temp_c: None,
            thermal: None,
            sampled_at: UNIX_EPOCH,
        };
        let v = serde_json::to_value(build(&model(false, None), Some(&metrics), UNIX_EPOCH)).unwrap();
        let m = v["metrics"].as_object().unwrap();
        assert_eq!(m["cpu_cores"], 8);
        assert_eq!(m["sampled_at"], "1970-01-01T00:00:00Z");
        for (k, val) in m {
            if k != "cpu_cores" && k != "sampled_at" {
                assert!(val.is_null(), "{k} should be null");
            }
        }
    }
}
