//! Synced folders (F43, board 56): one of a machine's shared folders, kept on this device.
//! One way, from the machine to here; nothing here is ever deleted. Each round lists the
//! folder (and its folders), works out what is new or changed since the last round
//! ([`Known`]), and downloads each such file into `.pgpu-partial/` first, so a dropped
//! connection carries on where it stopped and a half file is never in the folder. Every file
//! is checked against the machine's BLAKE3 hash before it is moved into place. A file that
//! was changed here since it came is kept: the new one gets a `(2)` name next to it.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use pocketgpu_proto::files::{Kind, Root, reserved_on_windows};
use serde::{Deserialize, Serialize};

use crate::{ClientError, Machine};

/// Where half-downloaded files wait, inside the synced folder.
pub const PARTIAL: &str = ".pgpu-partial";
/// At most this many files are looked at in one folder tree, so a huge folder can't stall
/// a round.
pub const MAX_FILES: usize = 20_000;
/// At most this deep.
pub const MAX_DEPTH: usize = 8;

/// A file on the machine: its `/`-separated path in the shared folder, size and time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub path: String,
    pub size: u64,
    pub modified_ms: u64,
}

/// What came in earlier rounds, by path: the size and time it had on the machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known(pub BTreeMap<String, Seen>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub size: u64,
    pub modified_ms: u64,
}

impl Known {
    /// Its size here when it last came: files land as exact copies, so the size it had on
    /// the machine then. `None` for a file that never came.
    pub fn landed(&self, path: &str) -> Option<u64> {
        self.0.get(path).map(|s| s.size)
    }

    /// Every file there now, as already had: "only new files" starts this way.
    pub fn all(remote: &[Remote]) -> Self {
        Self(
            remote
                .iter()
                .map(|r| {
                    (
                        r.path.clone(),
                        Seen {
                            size: r.size,
                            modified_ms: r.modified_ms,
                        },
                    )
                })
                .collect(),
        )
    }
}

/// The files to get this round: new, or changed on the machine since they came. Paths
/// that could leave the folder are skipped.
pub fn needed(remote: &[Remote], known: &Known) -> Vec<Remote> {
    remote
        .iter()
        .filter(|r| safe(&r.path).is_some())
        .filter(|r| match known.0.get(&r.path) {
            None => true,
            Some(s) => s.size != r.size || s.modified_ms != r.modified_ms,
        })
        .cloned()
        .collect()
}

/// How many files [`needed`] never gets because their names can't be saved here.
pub fn unsavable(remote: &[Remote]) -> usize {
    remote.iter().filter(|r| safe(&r.path).is_none()).count()
}

/// One look at a shared folder: its files, the folders too deep to look in, and whether it
/// stopped at [`MAX_FILES`].
#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub files: Vec<Remote>,
    pub deep: usize,
    pub full: bool,
}

/// `a/b.txt` as a relative path, or `None` for anything that could leave the folder, be
/// a drive or a device on Windows, or land in PocketGPU's own partial folder.
pub fn safe(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.split('/') {
        if !savable(part, cfg!(windows)) {
            return None;
        }
        out.push(part);
    }
    out.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(out)
}

/// Whether one part of a path can be saved as itself here. Everywhere: not empty, `.` or
/// `..`; no NUL, backslash, control character or direction override (with U+202E,
/// `photo` + U+202E + `gpj.exe` shows as `photoexe.jpg`); no leading dot (hidden names:
/// `.pgpu-partial`, `.ssh`; shared folders never list them) and no drive (`C:x`). On
/// Windows also none of `<>:"|?*` (`a:b` is a stream), no trailing dot or space and no
/// device name (`CON`, `nul.txt`). Other names Macs and Linux take as they are, so what
/// synced to them before still does; the disk refuses a name that is too long.
fn savable(part: &str, windows: bool) -> bool {
    let b = part.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let everywhere = !part.is_empty()
        && !part.starts_with('.')
        && !drive
        && !part.chars().any(|c| {
            c == '\\'
                || c.is_control()
                || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        });
    everywhere
        && !(windows
            && (part.contains(['<', '>', ':', '"', '|', '?', '*'])
                || part.ends_with(['.', ' '])
                || reserved_on_windows(part)))
}

/// `dir` joined with a checked relative path, or `None` when the result isn't inside it
/// (on Windows, joining a path with a drive replaces `dir`, and `name:x` is a hidden stream).
fn within(dir: &Path, rel: &Path) -> Option<PathBuf> {
    if cfg!(windows) && rel.as_os_str().to_string_lossy().contains(':') {
        return None;
    }
    let p = dir.join(rel);
    p.starts_with(dir).then_some(p)
}

/// Where a file goes in `dir`: its own path, or `name (2).ext` beside it when what is there
/// was changed here since it came (`landed`: its size here when it came).
pub fn target(dir: &Path, rel: &Path, landed: Option<u64>) -> PathBuf {
    let plain = dir.join(rel);
    let here = std::fs::metadata(&plain).ok().map(|m| m.len());
    match (here, landed) {
        (None, _) => plain,
        (Some(h), Some(l)) if h == l => plain,
        // There, and not what came last time (or never came from here): keep it.
        _ => free_name(&plain),
    }
}

fn free_name(p: &Path) -> PathBuf {
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| p.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .expect("a free name")
}

impl Machine {
    /// Every file in a shared folder and its folders, at most [`MAX_FILES`] and
    /// [`MAX_DEPTH`] deep.
    pub async fn sync_scan(&self, root: Root) -> Result<Vec<Remote>, ClientError> {
        Ok(self.sync_look(root).await?.files)
    }

    /// [`Machine::sync_scan`], also saying what it left out.
    pub async fn sync_look(&self, root: Root) -> Result<Scan, ClientError> {
        let mut out = Vec::new();
        let mut deep = 0;
        let mut folders = vec![(String::new(), 0usize)];
        while let Some((dir, depth)) = folders.pop() {
            let mut offset = 0;
            loop {
                let (entries, next) = self.files_list(root, &dir, offset).await?;
                for e in entries {
                    let path = if dir.is_empty() {
                        e.name.clone()
                    } else {
                        format!("{dir}/{}", e.name)
                    };
                    match e.kind {
                        Kind::Folder if depth + 1 < MAX_DEPTH => folders.push((path, depth + 1)),
                        Kind::Folder => deep += 1,
                        _ => out.push(Remote {
                            path,
                            size: e.size,
                            modified_ms: e.modified_ms,
                        }),
                    }
                    if out.len() >= MAX_FILES {
                        return Ok(Scan {
                            files: out,
                            deep,
                            full: true,
                        });
                    }
                }
                match next {
                    Some(n) => offset = n,
                    None => break,
                }
            }
        }
        Ok(Scan {
            files: out,
            deep,
            full: false,
        })
    }

    /// Gets one file into `dir`, carrying on from a partial copy, and returns where it
    /// landed and its size. `landed`: its size here when it came last time, if it did.
    pub async fn sync_get(
        &self,
        root: Root,
        file: &Remote,
        dir: &Path,
        landed: Option<u64>,
        progress: impl FnMut(u64, u64),
    ) -> Result<(PathBuf, u64), ClientError> {
        let rel = safe(&file.path)
            .ok_or_else(|| ClientError::Connect(format!("{} isn't a safe name", file.path)))?;
        let outside = || ClientError::Connect(format!("{} leads outside the folder", file.path));
        let partial = within(&dir.join(PARTIAL), &rel).ok_or_else(outside)?;
        if let Some(parent) = partial.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ClientError::File)?;
        }
        // A partial copy of an older version can't be carried on.
        if let Ok(m) = tokio::fs::metadata(&partial).await
            && m.len() > file.size
        {
            let _ = tokio::fs::remove_file(&partial).await;
        }
        let got = match self.download(root, &file.path, &partial, progress).await {
            Ok(d) => d,
            Err(e @ (ClientError::FileChanged | ClientError::HashMismatch)) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let dest = target(dir, &rel, landed);
        if !dest.starts_with(dir) {
            return Err(outside());
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ClientError::File)?;
        }
        tokio::fs::rename(&partial, &dest)
            .await
            .map_err(ClientError::File)?;
        // From another computer: SmartScreen and Gatekeeper ask before it runs.
        crate::mark_downloaded(&dest);
        Ok((dest, got.size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: &str, size: u64, t: u64) -> Remote {
        Remote {
            path: path.into(),
            size,
            modified_ms: t,
        }
    }

    #[test]
    fn new_and_changed_files_only() {
        let remote = [r("a.pt", 10, 1), r("b.pt", 20, 2), r("c/d.pt", 5, 3)];
        let mut known = Known::all(&remote[..2]);
        assert_eq!(needed(&remote, &known), vec![r("c/d.pt", 5, 3)]);
        known.0.get_mut("b.pt").unwrap().modified_ms = 1;
        let paths: Vec<_> = needed(&remote, &known)
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(paths, ["b.pt", "c/d.pt"]);
    }

    #[test]
    fn nothing_leaves_the_folder() {
        assert!(safe("../x").is_none());
        assert!(safe("/etc/passwd").is_none());
        assert!(safe("a/../../x").is_none());
        assert!(safe(".pgpu-partial/x").is_none());
        assert!(safe("").is_none());
        assert_eq!(safe("runs/a.pt"), Some(PathBuf::from("runs/a.pt")));
        let remote = [r("../x", 1, 1), r("a.pt", 1, 1)];
        assert_eq!(needed(&remote, &Known::default()), vec![r("a.pt", 1, 1)]);
        assert_eq!(unsavable(&remote), 1);
    }

    /// Whether every part of `path` passes, as Windows (`true`) or Macs and Linux see it.
    fn passes(path: &str, windows: bool) -> bool {
        path.split('/').all(|p| savable(p, windows))
    }

    #[test]
    fn no_drives_streams_or_devices() {
        // Refused everywhere. On Windows `C:x` is a path on drive C:, not a file in the folder.
        for bad in [
            "a/C:x",
            "a/D:x",
            "C:x",
            "d:/x",
            r"a\..\..\x",
            "a/x\0y",
            "a/x\ny",
            "a//b",
            "a/./b",
            ".ssh/authorized_keys",
            "a/.zshrc",
            "a/photo\u{202E}gpj.exe",
        ] {
            assert!(!passes(bad, false), "{bad:?}");
            assert!(!passes(bad, true), "{bad:?}");
            assert_eq!(safe(bad), None, "{bad:?}");
        }
        // Refused on Windows only: streams, device names, trailing dots and spaces.
        for windows_only in [
            "a/x.txt:Zone.Identifier",
            "run-2026-11-20T10:00:00/a.pt",
            "a/what?.txt",
            "a/CON",
            "a/nul.txt",
            "a/COM1.log",
            "a/x.",
            "a/x ",
        ] {
            assert!(passes(windows_only, false), "{windows_only:?}");
            assert!(!passes(windows_only, true), "{windows_only:?}");
        }
        for ok in ["a/b c/d.tar.gz", "Résumé.pdf", "COM10", "runs/v1.2/a.pt"] {
            assert!(passes(ok, true) && safe(ok).is_some(), "{ok:?}");
        }
        let remote = [r("a/C:x", 1, 1), r("a/D:x", 1, 1)];
        assert!(needed(&remote, &Known::default()).is_empty());
    }

    #[test]
    fn names_that_synced_before_still_do() {
        // Soft hyphens, zero-width spaces and names over 200 bytes (80 CJK characters).
        let long = format!("{}.png", "画".repeat(80));
        for ok in ["a/co\u{00AD}op.txt", "a/x\u{200B}y.txt", long.as_str()] {
            assert!(passes(ok, false) && passes(ok, true), "{ok:?}");
            assert!(safe(ok).is_some(), "{ok:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_never_replaces_the_folder() {
        let dir = Path::new(r"D:\Sync\runs");
        assert_eq!(within(dir, Path::new("C:x")), None);
        assert_eq!(within(dir, Path::new(r"a\D:x")), None);
        assert_eq!(
            within(dir, Path::new(r"a\b.txt")),
            Some(PathBuf::from(r"D:\Sync\runs\a\b.txt"))
        );
    }

    #[test]
    fn joins_stay_in_the_folder() {
        let dir = Path::new("/sync/runs");
        assert_eq!(within(dir, Path::new("/etc/x")), None);
        assert_eq!(
            within(dir, Path::new("a/b.txt")),
            Some(PathBuf::from("/sync/runs/a/b.txt"))
        );
    }

    #[test]
    fn a_file_changed_here_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let rel = Path::new("a.txt");
        assert_eq!(target(d.path(), rel, None), d.path().join("a.txt"));
        std::fs::write(d.path().join("a.txt"), b"12345").unwrap();
        // It came with 5 bytes and is still 5: the new version replaces it.
        assert_eq!(target(d.path(), rel, Some(5)), d.path().join("a.txt"));
        // Changed here (or never came from the machine): the new one goes beside it.
        assert_eq!(target(d.path(), rel, Some(3)), d.path().join("a (2).txt"));
        assert_eq!(target(d.path(), rel, None), d.path().join("a (2).txt"));
    }
}
