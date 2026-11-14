//! Taking a picture of a display (F45). Windows copies the screen with GDI, macOS asks the
//! system's `screencapture`, and Linux reads the X server's root window (Wayland desktops
//! can't be shown yet). `PGPU_FAKE_SCREEN=1` draws made-up displays instead, for tests and
//! machines without a desktop. Every call blocks: run it off the async threads.

mod fake;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;

use pocketgpu_proto::screen::Display;

/// A picture as taken: `width × height` pixels, 4 bytes each in blue, green, red, unused.
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Shot {
    /// The red, green, blue bytes of each pixel, for the encoder.
    pub fn rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height * 3) as usize);
        for [b, g, r, _] in self.bgra.as_chunks::<4>().0 {
            out.extend_from_slice(&[*r, *g, *b]);
        }
        out
    }
}

pub fn fake() -> bool {
    std::env::var("PGPU_FAKE_SCREEN").is_ok_and(|v| v == "1")
}

/// The displays here, or why there is nothing to show.
pub fn displays() -> Result<Vec<Display>, String> {
    if fake() {
        return Ok(fake::displays());
    }
    real_displays()
}

/// A picture of display `n` (0 is the first one [`displays`] gives).
pub fn grab(n: u32) -> Result<Shot, String> {
    if fake() {
        return fake::grab(n);
    }
    real_grab(n)
}

#[cfg(windows)]
fn real_displays() -> Result<Vec<Display>, String> {
    win::displays()
}
#[cfg(windows)]
fn real_grab(n: u32) -> Result<Shot, String> {
    win::grab(n)
}

#[cfg(target_os = "macos")]
fn real_displays() -> Result<Vec<Display>, String> {
    mac::displays()
}
#[cfg(target_os = "macos")]
fn real_grab(n: u32) -> Result<Shot, String> {
    mac::grab(n)
}

#[cfg(target_os = "linux")]
fn real_displays() -> Result<Vec<Display>, String> {
    linux::displays()
}
#[cfg(target_os = "linux")]
fn real_grab(n: u32) -> Result<Shot, String> {
    linux::grab(n)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn real_displays() -> Result<Vec<Display>, String> {
    Err("This system's screen can't be shown.".into())
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn real_grab(_n: u32) -> Result<Shot, String> {
    Err("This system's screen can't be shown.".into())
}

/// Asks the system once for leave to take pictures (macOS: Screen Recording).
pub fn ask_leave() {
    #[cfg(target_os = "macos")]
    if !fake() {
        mac::ask_leave();
    }
}
