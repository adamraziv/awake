//! RAM use and memory pressure from Mach VM statistics. Spec section 7.

use std::mem::MaybeUninit;

use super::{mach, sysctl};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPressure {
    Normal,
    Warn,
    Critical,
}

impl MemoryPressure {
    /// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
    pub fn from_level(level: u32) -> Option<Self> {
        match level {
            1 => Some(Self::Normal),
            2 => Some(Self::Warn),
            4 => Some(Self::Critical),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mem {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub pressure: Option<MemoryPressure>,
}

/// Used follows Activity Monitor: app memory (internal minus purgeable) plus wired plus compressed.
pub fn read() -> Option<Mem> {
    let mut stats = MaybeUninit::<libc::vm_statistics64>::uninit();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `stats` has room for HOST_VM_INFO64_COUNT integers, which is what the kernel writes.
    let rc = unsafe {
        libc::host_statistics64(mach::host(), libc::HOST_VM_INFO64, stats.as_mut_ptr().cast(), &mut count)
    };
    if rc != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: KERN_SUCCESS means the struct was filled.
    let s = unsafe { stats.assume_init() };
    // SAFETY: `vm_page_size` is a process-wide constant set at exec.
    let page = unsafe { libc::vm_page_size } as u64;
    let app = u64::from(s.internal_page_count).saturating_sub(u64::from(s.purgeable_count));
    let pages = app + u64::from(s.wire_count) + u64::from(s.compressor_page_count);
    Some(Mem {
        used_bytes: pages.saturating_mul(page),
        total_bytes: sysctl::by_name(c"hw.memsize")?,
        pressure: sysctl::by_name::<u32>(c"kern.memorystatus_vm_pressure_level").and_then(MemoryPressure::from_level),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_levels_map_to_the_three_states() {
        assert_eq!(MemoryPressure::from_level(1), Some(MemoryPressure::Normal));
        assert_eq!(MemoryPressure::from_level(2), Some(MemoryPressure::Warn));
        assert_eq!(MemoryPressure::from_level(4), Some(MemoryPressure::Critical));
        assert_eq!(MemoryPressure::from_level(0), None);
        assert_eq!(MemoryPressure::from_level(3), None);
        assert_eq!(MemoryPressure::Warn.as_str(), "warn");
    }

    #[test]
    fn mem_is_plausible() {
        let m = read().expect("host_statistics64");
        assert!(m.total_bytes >= 4 << 30, "total {}", m.total_bytes);
        // A booted macOS is never below a few hundred MB used, and used never exceeds total.
        assert!(m.used_bytes > 256 << 20 && m.used_bytes <= m.total_bytes, "{m:?}");
        assert!(m.pressure.is_some(), "pressure sysctl missing");
    }
}
