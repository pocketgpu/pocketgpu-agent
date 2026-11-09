//! Room for sends (FR-005): a send must leave 1 GB free on the disk, and each device has at
//! most 8 unfinished sends here, 64 GB in all. Sends running at the same time each hold
//! what they may still write, so they can't all count on the same free space or the same
//! 64 GB; while one runs, the free space is checked again every few megabytes, as other
//! programs fill the disk too.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use pocketgpu_proto::files::CHUNK;
use pocketgpu_proto::{Error, ErrorCode};

use crate::cli::files::size as shown_size;

/// Space a send must leave free.
pub const MIN_FREE: u64 = 1 << 30;
/// Unfinished sends per device.
pub const MAX_PARTIALS: usize = 8;
/// Bytes of unfinished sends per device, and so the biggest file a device can send.
pub const MAX_PARTIAL_BYTES: u64 = 64 << 30;
/// While a send runs, the free space is checked again after this many bytes.
pub const RECHECK: u64 = 16 * CHUNK as u64;

/// A send running now.
#[derive(Debug)]
struct Running {
    /// Its partial file's name.
    partial: String,
    /// The size it started with.
    size: u64,
    /// Bytes it may still write.
    left: u64,
}

static RUNNING: LazyLock<Mutex<HashMap<u64, Running>>> = LazyLock::new(Mutex::default);
static NEXT: AtomicU64 = AtomicU64::new(0);

/// The room one running send holds, given back when dropped.
#[derive(Debug)]
pub struct Reservation(u64);

impl Reservation {
    /// `n` more bytes of this send are on disk now.
    pub fn wrote(&self, n: u64) {
        let mut running = RUNNING.lock().expect("sends lock poisoned");
        if let Some(r) = running.get_mut(&self.0) {
            r.left = r.left.saturating_sub(n);
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        RUNNING.lock().expect("sends lock poisoned").remove(&self.0);
    }
}

/// Holds room for the partial `this` of `size` bytes with `have` there already, or says
/// why it doesn't fit. `dir` is the `.partial` folder, `prefix` the device's start of
/// partial names, `free` the disk's free bytes now.
pub fn reserve(
    dir: &Path,
    prefix: &str,
    this: &str,
    size: u64,
    have: u64,
    free: Option<u64>,
) -> Result<Reservation, Error> {
    let mut running = RUNNING.lock().expect("sends lock poisoned");
    let promised = running.values().map(|r| r.left).sum();
    disk_room(free, size - have, promised)?;
    let sizes: HashMap<String, u64> = running
        .values()
        .map(|r| (r.partial.clone(), r.size))
        .collect();
    partial_room(dir, prefix, this, size, &sizes)?;
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    running.insert(
        id,
        Running {
            partial: this.to_string(),
            size,
            left: size - have,
        },
    );
    Ok(Reservation(id))
}

/// Whether one more partial fits this device's share: at most [`MAX_PARTIALS`], and
/// [`MAX_PARTIAL_BYTES`] counting this one's whole size, the sizes running ones started
/// with (`running`, by partial name), and what the others hold.
pub fn partial_room(
    dir: &Path,
    prefix: &str,
    this: &str,
    size: u64,
    running: &HashMap<String, u64>,
) -> Result<(), Error> {
    if size > MAX_PARTIAL_BYTES {
        return Err(Error::new(
            ErrorCode::Invalid,
            "files over 64 GB can't be sent to this machine; split it into smaller parts first",
        ));
    }
    let mine = |name: &str| name != this && name.starts_with(prefix) && name.ends_with(".part");
    let mut held: HashMap<String, u64> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            mine(&name).then(|| (name, e.metadata().map_or(0, |m| m.len())))
        })
        .collect();
    // A running send can grow to the size it started with.
    for (name, size) in running {
        if mine(name) {
            held.insert(name.clone(), *size);
        }
    }
    if held.len() >= MAX_PARTIALS {
        return Err(Error::new(
            ErrorCode::Invalid,
            "8 unfinished sends from this device are waiting here; finish one first, or \
             wait a day for them to be removed",
        ));
    }
    let bytes = held.values().fold(size, |all, b| all.saturating_add(*b));
    if bytes > MAX_PARTIAL_BYTES {
        return Err(Error::new(
            ErrorCode::Invalid,
            "unfinished sends from this device would take over 64 GB here; finish one first",
        ));
    }
    Ok(())
}

/// Whether `need` more bytes still leave [`MIN_FREE`], with `promised` bytes held for other
/// sends running now; fine when the free space is unknown.
pub fn disk_room(free: Option<u64>, need: u64, promised: u64) -> Result<(), Error> {
    match free {
        Some(free) if free.saturating_sub(promised) < need.saturating_add(MIN_FREE) => {
            let held = if promised > 0 {
                format!(
                    ", {} of it for other sends still running",
                    shown_size(promised)
                )
            } else {
                String::new()
            };
            Err(Error::new(
                ErrorCode::Invalid,
                format!(
                    "not enough space: this needs {} and the machine keeps at least 1 GB free \
                     ({} free now{held})",
                    shown_size(need),
                    shown_size(free)
                ),
            ))
        }
        _ => Ok(()),
    }
}

/// While a send runs: whether the next [`RECHECK`] bytes still leave [`MIN_FREE`].
pub fn still_room(free: Option<u64>) -> Result<(), Error> {
    match free {
        Some(free) if free < MIN_FREE.saturating_add(RECHECK) => Err(Error::new(
            ErrorCode::Invalid,
            "the disk is nearly full, and the machine keeps at least 1 GB free, so this send \
             stopped; it carries on from here once there is room",
        )),
        _ => Ok(()),
    }
}

/// Free bytes on the disk holding `dir`.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // the field types differ between systems
pub fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: a NUL-terminated path and a zeroed struct the call fills.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0;
    ok.then(|| (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(windows)]
pub fn free_space(dir: &Path) -> Option<u64> {
    crate::winsys::free_space(dir).map(|(free, _)| free)
}

#[cfg(not(any(unix, windows)))]
pub fn free_space(_dir: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
