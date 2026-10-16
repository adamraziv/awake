//! Clamshell watcher: display off on lid close during a session. Spec section 4.6.

use std::ffi::c_void;

use objc2_core_foundation::{CFBoolean, CFRetained, CFRunLoop, CFString, CFType, kCFRunLoopCommonModes};
use objc2_io_kit::{
    IONotificationPort, IONotificationPortRef, IOObjectRelease, IORegistryEntryCreateCFProperty,
    IOServiceAddInterestNotification, IOServiceGetMatchingService, IOServiceMatching, io_object_t,
    io_service_t, kIOGeneralInterest, kIOMainPortDefault, kIOPMMessageClamshellStateChange,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clamshell {
    pub closed: bool,
    /// `AppleClamshellCausesSleep`: false means an external display is driving clamshell mode.
    pub causes_sleep: bool,
}

pub type LidCallback = Box<dyn Fn(Clamshell)>;

fn root_domain() -> Option<io_service_t> {
    // SAFETY: the class name is NUL-terminated; the matching dictionary is consumed by the call.
    let matching = unsafe { IOServiceMatching(c"IOPMrootDomain".as_ptr()) }?;
    let service = unsafe {
        IOServiceGetMatchingService(kIOMainPortDefault, Some(CFRetained::cast_unchecked(matching)))
    };
    if service == 0 { None } else { Some(service) }
}

fn bool_property(service: io_service_t, key: &str) -> Option<bool> {
    let key = CFString::from_str(key);
    // SAFETY: `service` is a valid registry entry for the duration of the call.
    let value: CFRetained<CFType> = unsafe { IORegistryEntryCreateCFProperty(service, Some(&key), None, 0) }?;
    Some(value.downcast_ref::<CFBoolean>()?.as_bool())
}

pub fn read_clamshell() -> Option<Clamshell> {
    let service = root_domain()?;
    let result = match (bool_property(service, "AppleClamshellState"), bool_property(service, "AppleClamshellCausesSleep")) {
        (Some(closed), Some(causes_sleep)) => Some(Clamshell { closed, causes_sleep }),
        _ => None,
    };
    IOObjectRelease(service);
    result
}

/// Owns the notification registration. Never dropped in practice; the app keeps it until exit.
pub struct LidWatcher {
    _port: IONotificationPortRef,
    _notification: io_object_t,
    _callback: *mut LidCallback,
}

unsafe extern "C-unwind" fn on_interest(refcon: *mut c_void, _service: io_service_t, message: u32, _arg: *mut c_void) {
    if message != kIOPMMessageClamshellStateChange {
        return;
    }
    // SAFETY: refcon is the leaked Box<LidCallback> from `watch`, alive for the process lifetime.
    let callback = unsafe { &*(refcon as *const LidCallback) };
    match read_clamshell() {
        Some(c) => {
            tracing::debug!("clamshell change: closed {} causes_sleep {}", c.closed, c.causes_sleep);
            callback(c);
        }
        None => tracing::warn!("clamshell change received but state unreadable"),
    }
}

/// Register for clamshell changes on IOPMrootDomain and deliver them on the main run loop.
/// Call from the main thread before `NSApplication::run`.
pub fn watch(callback: LidCallback) -> Result<LidWatcher, String> {
    let service = root_domain().ok_or("IOPMrootDomain not found")?;
    // SAFETY: kIOMainPortDefault is a constant mach port.
    let port = IONotificationPort::create(unsafe { kIOMainPortDefault });
    if port.is_null() {
        IOObjectRelease(service);
        return Err("IONotificationPortCreate failed".into());
    }
    let refcon = Box::into_raw(Box::new(callback));
    let mut notification: io_object_t = 0;
    // SAFETY: port and service are valid; the callback and refcon stay alive for the process.
    let rc = unsafe {
        IOServiceAddInterestNotification(
            port,
            service,
            kIOGeneralInterest.as_ptr() as *mut _,
            Some(on_interest),
            refcon as *mut c_void,
            &mut notification,
        )
    };
    IOObjectRelease(service);
    if rc != 0 {
        // SAFETY: nothing else holds refcon when registration failed.
        drop(unsafe { Box::from_raw(refcon) });
        return Err(format!("IOServiceAddInterestNotification failed: 0x{rc:x}"));
    }
    // SAFETY: port is a valid notification port.
    let source = unsafe { IONotificationPort::run_loop_source(port) }.ok_or("no run loop source")?;
    let run_loop = CFRunLoop::main().ok_or("no main run loop")?;
    // SAFETY: source and mode are valid; common modes keep delivery working while a menu is open.
    unsafe { run_loop.add_source(Some(&source), kCFRunLoopCommonModes) };
    tracing::info!("lid watcher registered");
    Ok(LidWatcher { _port: port, _notification: notification, _callback: refcon })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamshell_is_readable_on_host() {
        let c = read_clamshell().expect("IOPMrootDomain should expose AppleClamshellState on a MacBook");
        assert!(!c.closed, "the lid is open while tests run");
        let _ = c.causes_sleep;
    }

    #[test]
    fn interest_notification_registers() {
        let _watcher = watch(Box::new(|_| {})).expect("registration should succeed");
    }
}
