//! macOS: the display list from CoreGraphics, the picture from the system's `screencapture`
//! (no window, no sound). Pictures need Screen Recording for PocketGPU in System Settings →
//! Privacy & Security; without it the system gives the desktop picture only.

use std::process::Command;

use pocketgpu_proto::screen::Display;

use super::Shot;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

const NO_LEAVE: &str =
    "Allow PocketGPU in System Settings → Privacy & Security → Screen Recording on this Mac.";

fn ids() -> Vec<u32> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    // SAFETY: the buffer holds 16 ids and CoreGraphics writes at most `max` of them.
    let err = unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) };
    if err != 0 {
        return Vec::new();
    }
    ids[..count as usize].to_vec()
}

pub fn displays() -> Result<Vec<Display>, String> {
    // SAFETY: these CoreGraphics calls take no pointers.
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        return Err(NO_LEAVE.into());
    }
    let main = unsafe { CGMainDisplayID() };
    let list: Vec<Display> = ids()
        .into_iter()
        .enumerate()
        .map(|(i, id)| Display {
            name: format!("Display {}", i + 1),
            // SAFETY: as above.
            width: unsafe { CGDisplayPixelsWide(id) } as u32,
            height: unsafe { CGDisplayPixelsHigh(id) } as u32,
            primary: id == main,
        })
        .collect();
    if list.is_empty() {
        return Err("This Mac has no display on now.".into());
    }
    Ok(list)
}

pub fn ask_leave() {
    // SAFETY: no pointers; it shows the system's question once.
    unsafe {
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess();
        }
    }
}

pub fn grab(n: u32) -> Result<Shot, String> {
    let dir = tempfile_dir()?;
    let path = dir.join(format!("screen-{}-{n}.jpg", std::process::id()));
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-t", "jpg", "-D", &(n + 1).to_string()])
        .arg(&path)
        .status()
        .map_err(|e| format!("couldn't take a picture: {e}"))?;
    let bytes = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    if !status.success() {
        return Err(format!("there is no display {}", n + 1));
    }
    let img = image::load_from_memory_with_format(
        &bytes.map_err(|e| e.to_string())?,
        image::ImageFormat::Jpeg,
    )
    .map_err(|e| format!("couldn't read the picture: {e}"))?
    .to_rgba8();
    let (width, height) = img.dimensions();
    let mut bgra = img.into_raw();
    for p in bgra.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    Ok(Shot {
        width,
        height,
        bgra,
    })
}

/// A folder only this user can read, for the picture on its way.
fn tempfile_dir() -> Result<std::path::PathBuf, String> {
    let d = std::env::temp_dir().join(format!("pocketgpu-{}", unsafe { libc::getuid() }));
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    Ok(d)
}
