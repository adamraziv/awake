//! CPU die temperature through the private IOHIDEventSystemClient sensor interface, the path
//! macmon uses. Undocumented, so everything here is best-effort and behind `TempReader`.
//! Spec section 7.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};

pub trait TempReader {
    /// Average CPU die temperature in Celsius, or `None` when no usable sensor answered.
    fn cpu_temp_c(&mut self) -> Option<f32>;
}

/// For Macs without readable sensors, and for tests.
pub struct NoTemp;

impl TempReader for NoTemp {
    fn cpu_temp_c(&mut self) -> Option<f32> {
        None
    }
}

/// CPU die sensors are `pACC`/`eACC MTR Temp Sensor*` on M1 and `PMU tdie*`/`PMU2 tdie*` on
/// later chips (M1 has both; the ACC family is preferred). Readings outside 0..=130 C come from
/// disconnected probes and are dropped.
pub fn cpu_average(readings: &[(String, f64)]) -> Option<f32> {
    let family = |pick: fn(&str) -> bool| -> Vec<f64> {
        readings
            .iter()
            .filter(|(name, celsius)| pick(name) && (0.0..=130.0).contains(celsius))
            .map(|(_, celsius)| *celsius)
            .collect()
    };
    let mut temps = family(|name| name.contains("ACC MTR Temp Sensor"));
    if temps.is_empty() {
        temps = family(|name| name.starts_with("PMU tdie") || name.starts_with("PMU2 tdie"));
    }
    if temps.is_empty() {
        return None;
    }
    Some((temps.iter().sum::<f64>() / temps.len() as f64) as f32)
}

type ClientRef = *mut c_void;
type ServiceRef = *const c_void;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventSystemClientCreate(allocator: *const c_void) -> ClientRef;
    fn IOHIDEventSystemClientSetMatching(client: ClientRef, matching: *const CFDictionary) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: ClientRef) -> *mut CFArray;
    fn IOHIDServiceClientCopyProperty(service: ServiceRef, key: *const CFString) -> *mut CFType;
    fn IOHIDServiceClientCopyEvent(service: ServiceRef, event_type: i64, options: i32, timestamp: i64) -> *mut CFType;
    fn IOHIDEventGetFloatValue(event: *mut CFType, field: i32) -> f64;
}

const HID_PAGE_APPLE_VENDOR: i32 = 0xff00;
const HID_USAGE_APPLE_VENDOR_TEMPERATURE_SENSOR: i32 = 5;
const HID_EVENT_TYPE_TEMPERATURE: i64 = 15;
/// `IOHIDEventFieldBase(type)` is `type << 16`.
const HID_EVENT_FIELD_TEMPERATURE: i32 = (HID_EVENT_TYPE_TEMPERATURE << 16) as i32;

fn client_ptr(client: &CFRetained<CFType>) -> ClientRef {
    CFRetained::as_ptr(client).as_ptr().cast()
}

/// One event-system client matched to Apple vendor temperature sensors.
pub struct HidTemp {
    client: CFRetained<CFType>,
}

impl HidTemp {
    /// `None` when the private client cannot be created. Not `Send`: build it on the sampling thread.
    pub fn new() -> Option<Self> {
        // SAFETY: a null allocator means kCFAllocatorDefault; the result is a +1 CF object or null.
        let client = NonNull::new(unsafe { IOHIDEventSystemClientCreate(std::ptr::null()) })?;
        // SAFETY: we own the +1 reference just returned.
        let client: CFRetained<CFType> = unsafe { CFRetained::from_raw(client.cast()) };
        let keys = [CFString::from_str("PrimaryUsagePage"), CFString::from_str("PrimaryUsage")];
        let values = [
            CFNumber::new_i32(HID_PAGE_APPLE_VENDOR),
            CFNumber::new_i32(HID_USAGE_APPLE_VENDOR_TEMPERATURE_SENSOR),
        ];
        let matching: CFRetained<CFDictionary<CFString, CFNumber>> =
            CFDictionary::from_slices(&[&*keys[0], &*keys[1]], &[&*values[0], &*values[1]]);
        // SAFETY: both objects are live; the client copies the dictionary.
        unsafe { IOHIDEventSystemClientSetMatching(client_ptr(&client), CFRetained::as_ptr(&matching).as_ptr().cast()) };
        Some(Self { client })
    }

    /// Every matched sensor's product name and current reading in Celsius.
    pub fn readings(&self) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        // SAFETY: the client is live; CopyServices returns a +1 array of service clients or null.
        let Some(services) = NonNull::new(unsafe { IOHIDEventSystemClientCopyServices(client_ptr(&self.client)) })
        else {
            return out;
        };
        // SAFETY: we own the +1 reference just returned.
        let services: CFRetained<CFArray> = unsafe { CFRetained::from_raw(services) };
        let product = CFString::from_str("Product");
        for i in 0..services.count() {
            // SAFETY: `i` is in bounds; the elements are service clients owned by the array.
            let service: ServiceRef = unsafe { services.value_at_index(i) };
            // SAFETY: CopyProperty returns a +1 CF object or null, and we take ownership of it.
            let name = NonNull::new(unsafe { IOHIDServiceClientCopyProperty(service, &*product) })
                .map(|p| unsafe { CFRetained::<CFType>::from_raw(p) })
                .and_then(|p| p.downcast_ref::<CFString>().map(|s| s.to_string()))
                .unwrap_or_default();
            // SAFETY: CopyEvent returns a +1 IOHIDEvent (a CF object) or null.
            let Some(event) =
                NonNull::new(unsafe { IOHIDServiceClientCopyEvent(service, HID_EVENT_TYPE_TEMPERATURE, 0, 0) })
            else {
                continue;
            };
            // SAFETY: we own the +1 reference just returned; dropping it releases the event.
            let event: CFRetained<CFType> = unsafe { CFRetained::from_raw(event) };
            // SAFETY: the event is live and of the temperature type, so the field exists.
            let celsius =
                unsafe { IOHIDEventGetFloatValue(CFRetained::as_ptr(&event).as_ptr(), HID_EVENT_FIELD_TEMPERATURE) };
            out.push((name, celsius));
        }
        out
    }
}

impl TempReader for HidTemp {
    fn cpu_temp_c(&mut self) -> Option<f32> {
        cpu_average(&self.readings())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, celsius: f64) -> (String, f64) {
        (name.to_string(), celsius)
    }

    #[test]
    fn cpu_average_prefers_acc_sensors_and_drops_probes() {
        let m1 = [
            r("pACC MTR Temp Sensor3", 50.0),
            r("eACC MTR Temp Sensor0", 40.0),
            r("PMU tdie1", 70.0),
            r("PMU tdev2", -21.7),
            r("gas gauge battery", 29.0),
        ];
        assert_eq!(cpu_average(&m1), Some(45.0));
        let later = [r("PMU tdie1", 40.0), r("PMU2 tdie3", 50.0), r("PMU tdev1", -21.0), r("NAND CH0 temp", 35.0)];
        assert_eq!(cpu_average(&later), Some(45.0));
        let probes = [r("pACC MTR Temp Sensor3", -21.0), r("pACC MTR Temp Sensor4", 200.0)];
        assert_eq!(cpu_average(&probes), None);
        assert_eq!(cpu_average(&[]), None);
        assert_eq!(cpu_average(&[r("gas gauge battery", 29.0)]), None);
    }

    #[test]
    fn no_temp_is_none() {
        assert_eq!(NoTemp.cpu_temp_c(), None);
    }

    #[test]
    fn host_reader_is_best_effort() {
        // Spec 9: the temperature test accepts None. When the sensors answer, the value is a real
        // die temperature.
        let Some(mut reader) = HidTemp::new() else { return };
        let names = reader.readings();
        if let Some(t) = reader.cpu_temp_c() {
            assert!((0.0..=130.0).contains(&t), "temp {t} from {names:?}");
        }
    }
}
