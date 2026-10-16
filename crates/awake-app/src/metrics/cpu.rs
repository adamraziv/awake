//! CPU use from Mach per-processor tick counters. Spec section 7.

use std::mem::size_of;

use super::mach;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticks {
    pub busy: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cpu {
    pub cores: u32,
    /// Busy share of all cores since the previous sample, 0 to 100. `None` until two samples exist.
    pub percent: Option<f32>,
}

/// Busy share between two readings. `None` when no ticks elapsed or the counters went backwards.
pub fn percent_between(prev: Ticks, next: Ticks) -> Option<f32> {
    let total = next.total.checked_sub(prev.total)?;
    if total == 0 {
        return None;
    }
    let busy = next.busy.saturating_sub(prev.busy).min(total);
    Some(busy as f32 / total as f32 * 100.0)
}

fn read_ticks() -> Option<(u32, Ticks)> {
    let mut cpu_count: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = std::ptr::null_mut();
    let mut info_count: libc::mach_msg_type_number_t = 0;
    // SAFETY: the three out-pointers are valid; the kernel allocates `info` in this task.
    let rc = unsafe {
        libc::host_processor_info(
            mach::host(),
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut cpu_count,
            &mut info,
            &mut info_count,
        )
    };
    if rc != libc::KERN_SUCCESS || info.is_null() {
        return None;
    }
    // SAFETY: `info` holds `info_count` integers, CPU_STATE_MAX counters per processor.
    let counters = unsafe { std::slice::from_raw_parts(info.cast::<u32>(), info_count as usize) };
    let mut ticks = Ticks { busy: 0, total: 0 };
    let (per_cpu, _) = counters.as_chunks::<{ libc::CPU_STATE_MAX as usize }>();
    for cpu in per_cpu {
        let sum: u64 = cpu.iter().map(|&t| u64::from(t)).sum();
        ticks.busy += sum - u64::from(cpu[libc::CPU_STATE_IDLE as usize]);
        ticks.total += sum;
    }
    // SAFETY: the kernel allocated `info` for this task with exactly this many integers.
    unsafe {
        libc::vm_deallocate(
            mach::task_self(),
            info as libc::vm_address_t,
            info_count as usize * size_of::<libc::integer_t>(),
        )
    };
    Some((cpu_count, ticks))
}

/// Keeps the previous reading so each `sample` reports the busy share since the last one.
pub struct CpuSampler {
    prev: Option<Ticks>,
}

impl CpuSampler {
    /// Takes a first reading so the first `sample` after an interval already has a delta.
    pub fn new() -> Self {
        Self { prev: read_ticks().map(|(_, ticks)| ticks) }
    }

    pub fn sample(&mut self) -> Option<Cpu> {
        let (cores, now) = read_ticks()?;
        let percent = self.prev.and_then(|prev| percent_between(prev, now));
        self.prev = Some(now);
        Some(Cpu { cores, percent })
    }
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn percent_between_cases() {
        let a = Ticks { busy: 100, total: 1000 };
        assert_eq!(percent_between(a, Ticks { busy: 150, total: 1100 }), Some(50.0));
        assert_eq!(percent_between(a, Ticks { busy: 100, total: 1100 }), Some(0.0));
        assert_eq!(percent_between(a, Ticks { busy: 200, total: 1100 }), Some(100.0));
        // No time elapsed.
        assert_eq!(percent_between(a, a), None);
        // Counters went backwards (the kernel restarted counting).
        assert_eq!(percent_between(a, Ticks { busy: 0, total: 0 }), None);
        // Busy can never exceed total.
        assert_eq!(percent_between(a, Ticks { busy: 400, total: 1100 }), Some(100.0));
    }

    #[test]
    fn host_sample_is_plausible() {
        let mut s = CpuSampler::new();
        let first = s.sample().expect("host_processor_info");
        assert!(first.cores >= 1);
        // Ticks advance at 100 Hz per core, so 300 ms is plenty for a delta.
        std::thread::sleep(Duration::from_millis(300));
        let second = s.sample().expect("host_processor_info");
        assert_eq!(second.cores, first.cores);
        let p = second.percent.expect("ticks advanced over 300 ms");
        assert!((0.0..=100.0).contains(&p), "percent {p}");
    }
}
