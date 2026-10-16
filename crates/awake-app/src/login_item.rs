//! Launch at login through SMAppService.mainApp. Spec 3.1. Registration only works from
//! inside a signed .app bundle; from `cargo run` the status is NotFound and register fails.

use objc2_service_management::{SMAppService, SMAppServiceStatus};

pub fn status() -> SMAppServiceStatus {
    // SAFETY: plain query; no preconditions.
    unsafe { SMAppService::mainAppService().status() }
}

pub fn is_enabled() -> bool {
    status() == SMAppServiceStatus::Enabled
}

pub fn status_text() -> &'static str {
    match status() {
        SMAppServiceStatus::Enabled => "enabled",
        SMAppServiceStatus::NotRegistered => "not registered",
        SMAppServiceStatus::RequiresApproval => "requires approval in System Settings > Login Items",
        SMAppServiceStatus::NotFound => "not found (app is not running from a bundle)",
        _ => "unknown",
    }
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    // SAFETY: register/unregister have no preconditions beyond running inside a bundle,
    // and report that case through the returned NSError.
    let service = unsafe { SMAppService::mainAppService() };
    let result = if on {
        unsafe { service.registerAndReturnError() }
    } else {
        unsafe { service.unregisterAndReturnError() }
    };
    result.map_err(|e| e.localizedDescription().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_queryable_outside_a_bundle() {
        let _ = status_text();
        assert!(!is_enabled() || status() == SMAppServiceStatus::Enabled);
    }
}
