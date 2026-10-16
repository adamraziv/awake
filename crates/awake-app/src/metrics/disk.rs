//! Root volume usage from `statfs("/")`. Spec section 7.

use std::mem::MaybeUninit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disk {
    pub used_bytes: u64,
    pub total_bytes: u64,
}

/// Used is total minus what is available to the user. APFS reports container-wide block counts for
/// every volume, so this matches the Finder figure rather than the system volume alone.
pub fn read() -> Option<Disk> {
    let mut fs = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the path is NUL-terminated and `fs` is a valid out-pointer.
    if unsafe { libc::statfs(c"/".as_ptr(), fs.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statfs returned 0, so the struct is initialized.
    let fs = unsafe { fs.assume_init() };
    let bsize = u64::from(fs.f_bsize);
    Some(Disk {
        used_bytes: fs.f_blocks.saturating_sub(fs.f_bavail).saturating_mul(bsize),
        total_bytes: fs.f_blocks.saturating_mul(bsize),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_is_plausible() {
        let d = read().expect("statfs /");
        assert!(d.total_bytes > 8 << 30, "total {}", d.total_bytes);
        assert!(d.used_bytes > 0 && d.used_bytes <= d.total_bytes, "{d:?}");
    }
}
