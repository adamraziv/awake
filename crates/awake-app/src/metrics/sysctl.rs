//! Thin `sysctlbyname` wrapper for fixed-size values. Spec section 7.

use std::ffi::CStr;
use std::mem::{MaybeUninit, size_of};

/// Reads a fixed-size value such as `u64` or `libc::timeval`. `None` on error or size mismatch.
pub(crate) fn by_name<T: Copy>(name: &CStr) -> Option<T> {
    let mut value = MaybeUninit::<T>::uninit();
    let mut len = size_of::<T>();
    // SAFETY: `value` has room for `len` bytes and the kernel writes at most that many.
    let rc = unsafe {
        libc::sysctlbyname(name.as_ptr(), value.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
    };
    if rc != 0 || len != size_of::<T>() {
        return None;
    }
    // SAFETY: the kernel filled exactly `size_of::<T>()` bytes.
    Some(unsafe { value.assume_init() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_known_keys() {
        let ncpu: u32 = by_name(c"hw.ncpu").expect("hw.ncpu");
        assert!(ncpu >= 1);
        let memsize: u64 = by_name(c"hw.memsize").expect("hw.memsize");
        assert!(memsize > 1 << 30, "memsize {memsize}");
    }

    #[test]
    fn unknown_key_is_none() {
        assert_eq!(by_name::<u32>(c"awake.does.not.exist"), None);
    }

    #[test]
    fn wrong_size_is_none() {
        // hw.ncpu is 4 bytes. Asking for 8 must fail the size check, not return garbage.
        assert_eq!(by_name::<u64>(c"hw.ncpu"), None);
    }
}
