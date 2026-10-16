//! System metrics for the remote page. Sampled every 5 seconds on the tick thread and cached in
//! `App`; nothing here is shown locally. Spec section 7.

pub mod battery;
pub mod cpu;
pub mod disk;
mod mach;
pub mod mem;
mod sysctl;
pub mod temp;
pub mod thermal;
pub mod uptime;

use std::time::{Duration, SystemTime};

use crate::session::Battery;
use cpu::{Cpu, CpuSampler};
use disk::Disk;
use mem::Mem;
use temp::{HidTemp, NoTemp, TempReader};
use thermal::ThermalState;

pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// One sample. Every reader fails on its own: a failure is a `None` field, never an error.
#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    pub cpu: Option<Cpu>,
    pub mem: Option<Mem>,
    pub disk: Option<Disk>,
    pub battery: Option<Battery>,
    pub uptime: Option<Duration>,
    pub cpu_temp_c: Option<f32>,
    pub thermal: Option<ThermalState>,
    pub sampled_at: SystemTime,
}

/// Owns the stateful readers (CPU deltas, the HID client). Not `Send`: build it on the sampling thread.
pub struct Collector {
    cpu: CpuSampler,
    temp: Box<dyn TempReader>,
}

impl Collector {
    pub fn new(temp: Box<dyn TempReader>) -> Self {
        Self { cpu: CpuSampler::new(), temp }
    }

    /// Real readers. Logs once whether the temperature sensors answer; if they do not, the
    /// reader is replaced by `NoTemp` so no time is spent on them again.
    pub fn host() -> Self {
        let temp: Box<dyn TempReader> = match HidTemp::new() {
            Some(mut hid) => match hid.cpu_temp_c() {
                Some(t) => {
                    tracing::info!("cpu temperature sensors available ({t:.1} C)");
                    Box::new(hid)
                }
                None => {
                    tracing::warn!("cpu temperature unavailable: no CPU die sensor among {} readings", hid.readings().len());
                    Box::new(NoTemp)
                }
            },
            None => {
                tracing::warn!("cpu temperature unavailable: HID event system client not created");
                Box::new(NoTemp)
            }
        };
        Self::new(temp)
    }

    pub fn sample(&mut self) -> Metrics {
        Metrics {
            cpu: self.cpu.sample(),
            mem: mem::read(),
            disk: disk::read(),
            battery: battery::read(),
            uptime: uptime::read(),
            cpu_temp_c: self.temp.cpu_temp_c(),
            thermal: thermal::read(),
            sampled_at: SystemTime::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_sample_is_populated_on_host() {
        let mut c = Collector::new(Box::new(NoTemp));
        let m = c.sample();
        assert!(m.cpu.is_some(), "cpu {m:?}");
        assert!(m.mem.is_some(), "mem {m:?}");
        assert!(m.disk.is_some(), "disk {m:?}");
        assert!(m.uptime.is_some(), "uptime {m:?}");
        assert!(m.thermal.is_some(), "thermal {m:?}");
        assert_eq!(m.cpu_temp_c, None);
        assert!(m.sampled_at <= SystemTime::now());
    }

    #[test]
    fn collector_uses_the_given_temp_reader() {
        struct Fixed;
        impl TempReader for Fixed {
            fn cpu_temp_c(&mut self) -> Option<f32> {
                Some(42.5)
            }
        }
        let mut c = Collector::new(Box::new(Fixed));
        assert_eq!(c.sample().cpu_temp_c, Some(42.5));
    }
}
