//! GPU temperature from the IOHID temperature sensors, without root. Sensor names differ by
//! chip: GPU sensors are averaged, and the die sensors stand in when there are none.

use std::ffi::c_void;

use core_foundation_sys::base::{CFTypeRef, kCFAllocatorDefault};
use core_foundation_sys::dictionary::{
    CFDictionaryCreate, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
};
use core_foundation_sys::number::{CFNumberCreate, kCFNumberSInt32Type};
use core_foundation_sys::string::CFStringRef;

use super::cf::{Cf, cfstr, items, string};

/// kIOHIDEventTypeTemperature, and its value field.
const TEMPERATURE: i64 = 15;
const TEMPERATURE_LEVEL: i64 = TEMPERATURE << 16;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventSystemClientCreate(allocator: *const c_void) -> *const c_void;
    fn IOHIDEventSystemClientSetMatching(client: *const c_void, matching: CFTypeRef) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: *const c_void) -> CFTypeRef;
    fn IOHIDServiceClientCopyProperty(service: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDServiceClientCopyEvent(service: CFTypeRef, kind: i64, a: i32, b: i64) -> CFTypeRef;
    fn IOHIDEventGetFloatValue(event: CFTypeRef, field: i64) -> f64;
}

pub struct Temps {
    client: Cf,
}

impl Temps {
    pub fn new() -> Option<Self> {
        // SAFETY: CF and IOHID calls with valid arguments; created objects are owned.
        unsafe {
            let client = Cf::new(IOHIDEventSystemClientCreate(kCFAllocatorDefault.cast()))?;
            // Temperature sensors: usage page 0xff00, usage 5.
            let (page, usage) = (0xff00i32, 5i32);
            let keys = [cfstr("PrimaryUsagePage"), cfstr("PrimaryUsage")];
            let values = [
                Cf::new(
                    CFNumberCreate(
                        kCFAllocatorDefault,
                        kCFNumberSInt32Type,
                        (&raw const page).cast(),
                    )
                    .cast(),
                )?,
                Cf::new(
                    CFNumberCreate(
                        kCFAllocatorDefault,
                        kCFNumberSInt32Type,
                        (&raw const usage).cast(),
                    )
                    .cast(),
                )?,
            ];
            let key_refs = [keys[0].0, keys[1].0];
            let value_refs = [values[0].0, values[1].0];
            let matching = Cf::new(
                CFDictionaryCreate(
                    kCFAllocatorDefault,
                    key_refs.as_ptr(),
                    value_refs.as_ptr(),
                    2,
                    &kCFTypeDictionaryKeyCallBacks,
                    &kCFTypeDictionaryValueCallBacks,
                )
                .cast(),
            )?;
            IOHIDEventSystemClientSetMatching(client.0, matching.0);
            Some(Self { client })
        }
    }

    /// The GPU's temperature in °C.
    pub fn gpu_celsius(&self) -> Option<f64> {
        // SAFETY: a live client; copied services and events are owned and released.
        let services = Cf::new(unsafe { IOHIDEventSystemClientCopyServices(self.client.0) })?;
        let product = cfstr("Product");
        let (mut gpu, mut die) = (Vec::new(), Vec::new());
        for service in items(services.0) {
            // SAFETY: services are IOHIDServiceClient objects from the array above.
            let name =
                unsafe { Cf::new(IOHIDServiceClientCopyProperty(service, product.0.cast())) }
                    .and_then(|n| string(n.0));
            let Some(name) = name else { continue };
            let Some(event) =
                Cf::new(unsafe { IOHIDServiceClientCopyEvent(service, TEMPERATURE, 0, 0) })
            else {
                continue;
            };
            // SAFETY: a temperature event.
            let c = unsafe { IOHIDEventGetFloatValue(event.0, TEMPERATURE_LEVEL) };
            if !(c > 0.0 && c < 150.0) {
                continue;
            }
            if name.contains("GPU") {
                gpu.push(c);
            } else if name.contains("tdie") {
                die.push(c);
            }
        }
        let pick = if gpu.is_empty() { die } else { gpu };
        (!pick.is_empty()).then(|| pick.iter().sum::<f64>() / pick.len() as f64)
    }
}
