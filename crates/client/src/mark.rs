//! Marks a file that came from another computer as downloaded, as a browser does: the Mark
//! of the Web on Windows (SmartScreen, Office Protected View), `com.apple.quarantine` on
//! macOS (Gatekeeper). The agent marks what lands in its Inbox; the app marks what it saves
//! and what synced folders bring. Elsewhere nothing to do.

use std::path::Path;

/// Best effort: a disk that keeps no such marks (FAT, some network shares) keeps the file.
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn marks_a_file_as_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.exe");
        std::fs::write(&file, b"x").unwrap();
        mark_downloaded(&file);
        let out = std::process::Command::new("xattr")
            .arg("-p")
            .arg("com.apple.quarantine")
            .arg(&file)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains(";PocketGPU;"));
    }
}
