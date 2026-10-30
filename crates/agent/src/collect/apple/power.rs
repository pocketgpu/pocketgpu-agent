//! GPU power from IOReport's "Energy Model" group, without root (as `macmon` does). Energy
//! counters are read twice; the difference over the time between them is the power.

use std::ffi::c_void;
use std::time::Instant;

use core_foundation_sys::base::{CFTypeRef, kCFAllocatorDefault};
use core_foundation_sys::dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryRef, CFMutableDictionaryRef,
};
use core_foundation_sys::string::CFStringRef;

use super::cf::{Cf, cfstr, get, items, string};

#[link(name = "IOReport", kind = "dylib")]
unsafe extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CFStringRef,
        subgroup: CFStringRef,
        a: u64,
        b: u64,
        c: u64,
    ) -> CFDictionaryRef;
    fn IOReportCreateSubscription(
        a: *const c_void,
        channels: CFMutableDictionaryRef,
        subscribed: *mut CFMutableDictionaryRef,
        id: u64,
        b: CFTypeRef,
    ) -> *const c_void;
    fn IOReportCreateSamples(
        subscription: *const c_void,
        channels: CFMutableDictionaryRef,
        a: CFTypeRef,
    ) -> CFDictionaryRef;
    fn IOReportCreateSamplesDelta(
        previous: CFDictionaryRef,
        current: CFDictionaryRef,
        a: CFTypeRef,
    ) -> CFDictionaryRef;
    fn IOReportChannelGetGroup(item: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetChannelName(item: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetUnitLabel(item: CFDictionaryRef) -> CFStringRef;
    fn IOReportSimpleGetIntegerValue(item: CFDictionaryRef, index: i32) -> i64;
}

pub struct Power {
    subscription: *const c_void,
    channels: Cf,
    last: Option<(Cf, Instant)>,
}

// SAFETY: only used behind the collector's mutex, one thread at a time.
unsafe impl Send for Power {}

impl Power {
    /// `None` when IOReport has no energy channels (a virtual machine, say).
    pub fn new() -> Option<Self> {
        let group = cfstr("Energy Model");
        // SAFETY: IOReport calls with valid CF arguments; returned objects are owned.
        unsafe {
            let found = Cf::new(
                IOReportCopyChannelsInGroup(group.0.cast(), std::ptr::null(), 0, 0, 0).cast(),
            )?;
            let size = CFDictionaryGetCount(found.0.cast());
            let channels = Cf::new(
                CFDictionaryCreateMutableCopy(kCFAllocatorDefault, size, found.0.cast()).cast(),
            )?;
            let mut subscribed: CFMutableDictionaryRef = std::ptr::null_mut();
            let subscription = IOReportCreateSubscription(
                std::ptr::null(),
                channels.0 as CFMutableDictionaryRef,
                &mut subscribed,
                0,
                std::ptr::null(),
            );
            if subscription.is_null() {
                return None;
            }
            // The subscribed copy is what samples are taken from.
            let channels = Cf::new(subscribed.cast())?;
            Some(Self {
                subscription,
                channels,
                last: None,
            })
        }
    }

    /// Watts since the previous call; `None` on the first call.
    pub fn gpu_watts(&mut self) -> Option<f64> {
        // SAFETY: a live subscription and its channel dictionary.
        let now = Cf::new(
            unsafe {
                IOReportCreateSamples(
                    self.subscription,
                    self.channels.0 as CFMutableDictionaryRef,
                    std::ptr::null(),
                )
            }
            .cast(),
        )?;
        let at = Instant::now();
        let (previous, then) = self.last.replace((now, at))?;
        let current = &self.last.as_ref().expect("just set").0;
        // SAFETY: both are sample dictionaries from this subscription.
        let delta = Cf::new(
            unsafe {
                IOReportCreateSamplesDelta(previous.0.cast(), current.0.cast(), std::ptr::null())
            }
            .cast(),
        )?;
        let secs = at.duration_since(then).as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        let mut joules = None;
        for item in items(get(delta.0, "IOReportChannels")) {
            let item = item as CFDictionaryRef;
            // SAFETY: items of a samples dictionary are channel dictionaries; the getters
            // return borrowed strings.
            let (group, name, unit, value) = unsafe {
                (
                    string(IOReportChannelGetGroup(item).cast()),
                    string(IOReportChannelGetChannelName(item).cast()),
                    string(IOReportChannelGetUnitLabel(item).cast()),
                    IOReportSimpleGetIntegerValue(item, 0),
                )
            };
            if group.as_deref() != Some("Energy Model")
                || !name.as_deref().is_some_and(|n| n.ends_with("GPU Energy"))
            {
                continue;
            }
            let scale = match unit.as_deref().map(str::trim) {
                Some("mJ") => 1e3,
                Some("uJ") => 1e6,
                Some("nJ") => 1e9,
                _ => continue,
            };
            *joules.get_or_insert(0.0) += value as f64 / scale;
        }
        joules.map(|j: f64| j / secs)
    }
}
