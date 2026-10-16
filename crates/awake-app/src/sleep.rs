//! IOKit idle-sleep assertion and the SleepController that uses it plus the helper. Spec 2, 4.1, 4.2.

use std::sync::Arc;

use awake_proto::Request;
use objc2_core_foundation::CFString;
use objc2_io_kit::{
    IOPMAssertionCreateWithName, IOPMAssertionID, IOPMAssertionRelease, kIOPMAssertionLevelOn,
    kIOReturnSuccess,
};

use crate::helper::HelperClient;
use crate::session::SleepController;

pub const ASSERTION_TYPE: &str = "PreventUserIdleSystemSleep";
pub const ASSERTION_NAME: &str = "Awake session";

/// Holds a PreventUserIdleSystemSleep assertion. Dropping it releases the assertion.
pub struct IdleSleepAssertion(IOPMAssertionID);

impl IdleSleepAssertion {
    pub fn create() -> Result<Self, String> {
        let mut id: IOPMAssertionID = 0;
        let kind = CFString::from_static_str(ASSERTION_TYPE);
        let name = CFString::from_static_str(ASSERTION_NAME);
        // SAFETY: both strings outlive the call and `id` is a valid out-pointer.
        let rc = unsafe { IOPMAssertionCreateWithName(Some(&kind), kIOPMAssertionLevelOn, Some(&name), &mut id) };
        if rc == kIOReturnSuccess {
            Ok(Self(id))
        } else {
            Err(format!("IOPMAssertionCreateWithName failed: 0x{rc:x}"))
        }
    }

    pub fn id(&self) -> IOPMAssertionID {
        self.0
    }
}

impl Drop for IdleSleepAssertion {
    fn drop(&mut self) {
        let _ = IOPMAssertionRelease(self.0);
    }
}

/// The production SleepController: IOKit assertion plus the helper socket.
pub struct RealSleep {
    assertion: Option<IdleSleepAssertion>,
    helper: Arc<HelperClient>,
}

impl RealSleep {
    pub fn new(helper: Arc<HelperClient>) -> Self {
        Self { assertion: None, helper }
    }
}

impl SleepController for RealSleep {
    fn assert_idle_sleep(&mut self) -> Result<(), String> {
        if self.assertion.is_none() {
            self.assertion = Some(IdleSleepAssertion::create()?);
        }
        Ok(())
    }

    fn release_idle_sleep(&mut self) {
        self.assertion = None;
    }

    fn helper_sleep_disable(&mut self) -> Result<(), String> {
        self.helper.request_ok(Request::SleepDisable).map_err(|e| e.to_string())
    }

    fn helper_sleep_enable(&mut self) {
        if let Err(e) = self.helper.request_ok(Request::SleepEnable) {
            tracing::warn!("sleep_enable not delivered ({e}); the helper's disconnect reset covers it");
        }
    }

    fn helper_shutdown(&mut self) -> Result<(), String> {
        self.helper.request_ok(Request::Shutdown).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SleepController;

    fn assertions_list() -> String {
        let out = std::process::Command::new("/usr/bin/pmset").args(["-g", "assertions"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn assertion_is_visible_while_held() {
        let a = IdleSleepAssertion::create().unwrap();
        assert_ne!(a.id(), 0);
        assert!(assertions_list().contains(ASSERTION_NAME), "pmset -g assertions should list the assertion");
        drop(a);
    }

    #[test]
    fn real_sleep_is_idempotent_and_survives_missing_helper() {
        let helper = Arc::new(HelperClient::new("/nonexistent/awake-test.sock"));
        let mut s = RealSleep::new(helper);
        s.assert_idle_sleep().unwrap();
        let first = s.assertion.as_ref().unwrap().id();
        s.assert_idle_sleep().unwrap();
        assert_eq!(s.assertion.as_ref().unwrap().id(), first, "second assert must not create a new assertion");
        assert!(s.helper_sleep_disable().is_err());
        s.helper_sleep_enable();
        s.release_idle_sleep();
        assert!(s.assertion.is_none());
        s.release_idle_sleep();
    }
}
