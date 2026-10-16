//! Mach ports shared by the cpu and mem readers. `mach_host_self` returns a new send right each
//! call, so it is taken once and reused; `libc`'s binding is deprecated, hence the local extern.

use std::sync::OnceLock;

unsafe extern "C" {
    fn mach_host_self() -> libc::mach_port_t;
    static mach_task_self_: libc::mach_port_t;
}

pub(crate) fn host() -> libc::mach_port_t {
    static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
    // SAFETY: a Mach trap with no arguments and no preconditions.
    *HOST.get_or_init(|| unsafe { mach_host_self() })
}

pub(crate) fn task_self() -> libc::mach_port_t {
    // SAFETY: a process-wide constant the loader sets before `main`.
    unsafe { mach_task_self_ }
}
