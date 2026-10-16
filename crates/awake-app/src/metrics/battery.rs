//! Battery state from IOKit power sources. Spec sections 4.4 and 7.

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFString, CFType};
use objc2_io_kit::{
    IOPSCopyPowerSourcesInfo, IOPSCopyPowerSourcesList, IOPSGetPowerSourceDescription,
    IOPSGetProvidingPowerSourceType, kIOPSACPowerValue, kIOPSCurrentCapacityKey, kIOPSIsChargingKey,
};

use crate::session::Battery;

/// First power source with a current capacity (the internal battery on a laptop).
pub fn read() -> Option<Battery> {
    let blob = IOPSCopyPowerSourcesInfo()?;
    // SAFETY: `blob` is a live snapshot for every call below.
    let on_ac = unsafe { IOPSGetProvidingPowerSourceType(Some(&blob)) }
        .map(|s| s.to_string() == kIOPSACPowerValue.to_str().unwrap_or(""))
        .unwrap_or(false);
    let list = unsafe { IOPSCopyPowerSourcesList(Some(&blob)) }?;
    let capacity_key = CFString::from_str(kIOPSCurrentCapacityKey.to_str().ok()?);
    let charging_key = CFString::from_str(kIOPSIsChargingKey.to_str().ok()?);
    for i in 0..list.count() {
        // SAFETY: `i` is within bounds and the array holds CF objects owned by `blob`.
        let source = unsafe { list.value_at_index(i) };
        let source: &CFType = unsafe { &*(source as *const CFType) };
        let Some(desc) = (unsafe { IOPSGetPowerSourceDescription(Some(&blob), Some(source)) }) else {
            continue;
        };
        // SAFETY: power source descriptions are CFString-keyed dictionaries of CF values.
        let desc: &CFDictionary<CFString, CFType> = unsafe { desc.cast_unchecked() };
        let Some(percent) = desc.get(&capacity_key).and_then(|v| v.downcast_ref::<CFNumber>()?.as_i64()) else {
            continue;
        };
        let charging = desc
            .get(&charging_key)
            .and_then(|v| v.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool))
            .unwrap_or(false);
        return Some(Battery { percent: percent.clamp(0, 100) as u8, charging, on_ac });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_is_plausible_on_host() {
        // A desktop Mac has no battery and returns None; a laptop returns a percentage.
        if let Some(b) = read() {
            assert!(b.percent <= 100, "percent {}", b.percent);
            if b.charging {
                assert!(b.on_ac, "charging implies on AC");
            }
        }
    }
}
