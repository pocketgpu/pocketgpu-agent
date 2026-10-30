//! Small CoreFoundation and IOKit helpers: owned CF values, reading numbers and strings
//! out of dictionaries, and walking the IO registry.

use std::ffi::{CString, c_char, c_void};

use core_foundation_sys::array::{
    CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFArrayRef,
};
use core_foundation_sys::base::{CFGetTypeID, CFRelease, CFTypeRef, kCFAllocatorDefault};
use core_foundation_sys::dictionary::{
    CFDictionaryGetTypeID, CFDictionaryGetValue, CFDictionaryRef,
};
use core_foundation_sys::number::{
    CFNumberGetTypeID, CFNumberGetValue, CFNumberRef, kCFNumberSInt64Type,
};
use core_foundation_sys::string::{
    CFStringCreateWithCString, CFStringGetCString, CFStringGetTypeID, CFStringRef,
    kCFStringEncodingUTF8,
};

pub type IoObject = u32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main: u32, matching: *const c_void, it: *mut IoObject) -> i32;
    fn IOIteratorNext(it: IoObject) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntryGetChildIterator(
        entry: IoObject,
        plane: *const c_char,
        it: *mut IoObject,
    ) -> i32;
}

/// A CF value we own (from a Create or Copy call), released on drop.
pub struct Cf(pub CFTypeRef);

impl Cf {
    /// `None` for a null result.
    pub fn new(r: CFTypeRef) -> Option<Self> {
        // Not `then_some(Self(r))`: that builds the value first, so a null result was dropped
        // and CFRelease(NULL) crashed the agent.
        if r.is_null() { None } else { Some(Self(r)) }
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: we own one reference to a non-null CF value.
        unsafe { CFRelease(self.0) }
    }
}

// SAFETY: CF values are reference counted with atomic counts; we never share mutable ones.
unsafe impl Send for Cf {}

/// A CF string for `s`, owned.
pub fn cfstr(s: &str) -> Cf {
    let c = CString::new(s).expect("no NUL in CF string literals");
    // SAFETY: a valid C string; the result is owned (Create rule).
    let r = unsafe {
        CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), kCFStringEncodingUTF8)
    };
    Cf::new(r.cast()).expect("CFStringCreateWithCString")
}

/// `value` if it is a CFNumber, as an i64.
pub fn number(value: CFTypeRef) -> Option<i64> {
    // SAFETY: the type is checked before reading the number.
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: i64 = 0;
        CFNumberGetValue(
            value as CFNumberRef,
            kCFNumberSInt64Type,
            (&raw mut out).cast(),
        )
        .then_some(out)
    }
}

/// `value` if it is a CFString.
pub fn string(value: CFTypeRef) -> Option<String> {
    // SAFETY: the type is checked; the buffer is sized and NUL-terminated by CF.
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let mut buf = [0 as c_char; 256];
        (CFStringGetCString(
            value as CFStringRef,
            buf.as_mut_ptr(),
            buf.len() as _,
            kCFStringEncodingUTF8,
        ) != 0)
            .then(|| {
                std::ffi::CStr::from_ptr(buf.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            })
    }
}

/// `dict[key]`, borrowed from the dictionary (not owned).
pub fn get(dict: CFTypeRef, key: &str) -> CFTypeRef {
    // SAFETY: the type is checked; the key is a valid CF string kept alive for the call.
    unsafe {
        if dict.is_null() || CFGetTypeID(dict) != CFDictionaryGetTypeID() {
            return std::ptr::null();
        }
        let k = cfstr(key);
        CFDictionaryGetValue(dict as CFDictionaryRef, k.0)
    }
}

/// The items of `value` if it is a CFArray, borrowed.
pub fn items(value: CFTypeRef) -> Vec<CFTypeRef> {
    // SAFETY: the type is checked; indexes stay below the count.
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFArrayGetTypeID() {
            return Vec::new();
        }
        let a = value as CFArrayRef;
        (0..CFArrayGetCount(a))
            .map(|i| CFArrayGetValueAtIndex(a, i))
            .collect()
    }
}

/// An IO registry entry we hold, released on drop.
pub struct Entry(pub IoObject);

impl Drop for Entry {
    fn drop(&mut self) {
        // SAFETY: we hold one reference to this registry object.
        unsafe { IOObjectRelease(self.0) };
    }
}

impl Entry {
    /// A copy of the property `key`.
    pub fn property(&self, key: &str) -> Option<Cf> {
        let k = cfstr(key);
        // SAFETY: a held entry and a valid key; the result is owned (Create rule).
        Cf::new(unsafe {
            IORegistryEntryCreateCFProperty(self.0, k.0.cast(), kCFAllocatorDefault.cast(), 0)
        })
    }

    /// The entry's direct children in the IOService plane.
    pub fn children(&self) -> Vec<Entry> {
        let mut it: IoObject = 0;
        // SAFETY: a held entry; the iterator is released below.
        unsafe {
            if IORegistryEntryGetChildIterator(self.0, c"IOService".as_ptr(), &mut it) != 0 {
                return Vec::new();
            }
            drain(it)
        }
    }
}

/// Every registered service of IOKit class `class`.
pub fn services(class: &str) -> Vec<Entry> {
    let c = CString::new(class).expect("no NUL in class names");
    let mut it: IoObject = 0;
    // SAFETY: IOServiceGetMatchingServices consumes the matching dictionary.
    unsafe {
        let matching = IOServiceMatching(c.as_ptr());
        if matching.is_null() || IOServiceGetMatchingServices(0, matching, &mut it) != 0 {
            return Vec::new();
        }
        drain(it)
    }
}

/// Takes every object out of an iterator, then releases it.
unsafe fn drain(it: IoObject) -> Vec<Entry> {
    let mut out = Vec::new();
    loop {
        // SAFETY: a valid iterator; each object returned is ours to release.
        let next = unsafe { IOIteratorNext(it) };
        if next == 0 {
            break;
        }
        out.push(Entry(next));
    }
    // SAFETY: the iterator is ours.
    unsafe { IOObjectRelease(it) };
    out
}

/// A sysctl value by name: a string or a 64-bit number.
pub fn sysctl_string(name: &str) -> Option<String> {
    let c = CString::new(name).ok()?;
    let mut buf = [0u8; 256];
    let mut len = buf.len();
    // SAFETY: the buffer and its length are passed together.
    let rc = unsafe {
        libc::sysctlbyname(
            c.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then(|| {
        String::from_utf8_lossy(&buf[..len])
            .trim_end_matches('\0')
            .trim()
            .to_string()
    })
}

pub fn sysctl_u64(name: &str) -> Option<u64> {
    let c = CString::new(name).ok()?;
    let mut value: u64 = 0;
    let mut len = size_of::<u64>();
    // SAFETY: the value and its length are passed together.
    let rc = unsafe {
        libc::sysctlbyname(
            c.as_ptr(),
            (&raw mut value).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == size_of::<u64>()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apple APIs return null for anything missing; that must be `None`, not a crash.
    #[test]
    fn null_is_none_and_never_released() {
        assert!(Cf::new(std::ptr::null()).is_none());
        let missing = services("IOAccelerator")
            .into_iter()
            .next()
            .and_then(|gpu| gpu.property("no-such-property"));
        assert!(missing.is_none());
    }
}
