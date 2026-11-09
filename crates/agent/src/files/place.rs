//! Where a finished send goes in the Inbox (FR-004): a name nothing else has, never over
//! another file, marked as downloaded as a browser would, and noted for a day.

use std::io::ErrorKind;
use std::path::Path;

use pocketgpu_proto::files::clean_name;

/// Gives the partial a free name in the Inbox and removes the partial: a hard link fails
/// when the name is taken (also in another case, where the disk ignores case), so nothing
/// is ever replaced. Where links can't be made (FAT and exFAT disks, some network shares,
/// each with its own error), the name is taken with `create_new` first. A partial or Inbox
/// that is gone is an error as it is.
pub fn place(partial: &Path, inbox: &Path, wanted: &str) -> std::io::Result<String> {
    let base = clean_name(wanted);
    for n in 1..=MAX_TRIES {
        let name = numbered(&base, n);
        match std::fs::hard_link(partial, inbox.join(&name)) {
            Ok(()) => {
                std::fs::remove_file(partial)?;
                return Ok(name);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(e),
            Err(_) => return place_by_rename(partial, inbox, &base, n),
        }
    }
    Err(std::io::Error::other("no free name for the file"))
}

const MAX_TRIES: u32 = 9999;

fn place_by_rename(partial: &Path, inbox: &Path, base: &str, from: u32) -> std::io::Result<String> {
    for n in from..=MAX_TRIES {
        let name = numbered(base, n);
        let dest = inbox.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
        {
            Ok(_) => {
                // Replaces only the empty file just made for this name, which goes again
                // when the partial can't take its place.
                if let Err(e) = std::fs::rename(partial, &dest) {
                    let _ = std::fs::remove_file(&dest);
                    return Err(e);
                }
                return Ok(name);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free name for the file"))
}

/// `photo.jpg`, `photo (2).jpg`, `data (3).tar.gz`.
pub fn numbered(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    format!("{stem} ({n}){ext}")
}

fn split_ext(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for double in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(double) && name.len() > double.len() {
            return name.split_at(name.len() - double.len());
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

/// Marks a sent file as coming from elsewhere, as a browser does: the Mark of the Web on
/// Windows, `com.apple.quarantine` on macOS. Best effort.
pub fn mark_downloaded(path: &Path) {
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let _ = std::fs::write(
            std::path::PathBuf::from(stream),
            "[ZoneTransfer]\r\nZoneId=3\r\n",
        );
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let value = format!("0081;{secs:x};PocketGPU;");
        if let Ok(p) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
            // SAFETY: NUL-terminated path and name; the value's pointer and length come
            // from one string.
            unsafe {
                libc::setxattr(
                    p.as_ptr(),
                    c"com.apple.quarantine".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                );
            }
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = path;
}

/// The note of a finished send, `<hash in hex>\n<name>\n`, so a device that missed the
/// reply hears it again. Best effort: without it a retry sends the file again.
pub fn write_note(note: &Path, name: &str, hash: &[u8; 32]) {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    if let Err(e) = std::fs::write(note, format!("{hex}\n{name}\n")) {
        tracing::warn!("could not note a finished send: {e}");
    }
}

pub fn read_note(note: &Path) -> Option<(String, [u8; 32])> {
    let text = std::fs::read_to_string(note).ok()?;
    let mut lines = text.lines();
    let hex = lines.next()?;
    let name = lines.next().filter(|n| !n.is_empty())?;
    if hex.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    for (i, b) in hash.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some((name.to_string(), hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbox() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("pgpu-place-")
            .tempdir()
            .unwrap();
        let inbox = dir.path().join("Inbox");
        std::fs::create_dir_all(inbox.join(".partial")).unwrap();
        (dir, inbox)
    }

    fn partial(inbox: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = inbox.join(".partial").join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn names_never_replace_another_file() {
        let (_dir, inbox) = inbox();
        std::fs::write(inbox.join("photo.jpg"), "old").unwrap();
        std::fs::write(inbox.join("photo (2).jpg"), "old").unwrap();
        let p = partial(&inbox, "a-1.part", b"new");
        assert_eq!(place(&p, &inbox, "photo.jpg").unwrap(), "photo (3).jpg");
        assert!(!p.exists());
        assert_eq!(std::fs::read(inbox.join("photo.jpg")).unwrap(), b"old");
        assert_eq!(std::fs::read(inbox.join("photo (3).jpg")).unwrap(), b"new");

        // The name is cleaned first: no folders, no hidden files.
        let p = partial(&inbox, "a-2.part", b"x");
        assert_eq!(place(&p, &inbox, "../../.bashrc").unwrap(), "_bashrc");
    }

    #[test]
    fn numbers_go_before_the_extension() {
        assert_eq!(numbered("photo.jpg", 1), "photo.jpg");
        assert_eq!(numbered("photo.jpg", 2), "photo (2).jpg");
        assert_eq!(numbered("data.tar.gz", 3), "data (3).tar.gz");
        assert_eq!(numbered("README", 2), "README (2)");
        assert_eq!(numbered("my.photo.png", 2), "my.photo (2).png");
    }

    #[test]
    fn rename_takes_a_free_name_where_links_fail() {
        let (_dir, inbox) = inbox();
        std::fs::write(inbox.join("a.txt"), "old").unwrap();
        let p = partial(&inbox, "a-1.part", b"new");
        assert_eq!(
            place_by_rename(&p, &inbox, "a.txt", 1).unwrap(),
            "a (2).txt"
        );
        assert_eq!(std::fs::read(inbox.join("a (2).txt")).unwrap(), b"new");
        assert_eq!(std::fs::read(inbox.join("a.txt")).unwrap(), b"old");
    }

    #[test]
    fn a_partial_that_is_gone_leaves_nothing_behind() {
        let (_dir, inbox) = inbox();
        std::fs::write(inbox.join("video.mp4"), "saved").unwrap();
        let gone = inbox.join(".partial").join("gone.part");
        let e = place(&gone, &inbox, "video.mp4").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::NotFound);
        // Where rename is used, its placeholder goes again too.
        assert!(place_by_rename(&gone, &inbox, "video.mp4", 1).is_err());
        let names: Vec<_> = std::fs::read_dir(&inbox)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".partial")
            .collect();
        assert_eq!(names, ["video.mp4"]);
    }
}
