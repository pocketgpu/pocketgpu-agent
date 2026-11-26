//! Synced folders (F43, board 56): one of a machine's shared folders, kept on this device.
//! One way, from the machine to here; nothing here is ever deleted. Each round lists the
//! folder (and its folders), works out what is new or changed since the last round
//! ([`Known`]), and downloads each such file into `.pgpu-partial/` first, so a dropped
//! connection carries on where it stopped and a half file is never in the folder. Every file
//! is checked against the machine's BLAKE3 hash before it is moved into place. A file that
//! was changed here since it came is kept: the new one gets a `(2)` name next to it.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

use pocketgpu_proto::ErrorCode;
use pocketgpu_proto::files::{Entry, Kind, Root, reserved_on_windows};
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

impl Remote {
    /// As had, with no copy here: "only new files", and what a phone leaves on the machine.
    pub fn seen(&self) -> Seen {
        Seen {
            size: self.size,
            modified_ms: self.modified_ms,
            here_ms: None,
        }
    }
}

/// What came in earlier rounds, by path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known(pub BTreeMap<String, Seen>);

/// A file as it came: the size and time it had on the machine when it was sent (files land
/// as exact copies, so also its size here), and its time here once it landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub size: u64,
    pub modified_ms: u64,
    /// The copy's last change here as it landed (Unix ms), so an edit here that keeps the
    /// size is seen too. `None` when no copy landed here, or it came before this was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub here_ms: Option<u64>,
}

impl Known {
    /// How it was when it last came; `None` for a file that never came.
    pub fn landed(&self, path: &str) -> Option<Seen> {
        self.0.get(path).copied()
    }

    /// Every file there now, as already had: "only new files" starts this way.
    pub fn all(remote: &[Remote]) -> Self {
        Self(remote.iter().map(|r| (r.path.clone(), r.seen())).collect())
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
/// was changed here since it came (`landed`: how it was then): another size or time.
pub fn target(dir: &Path, rel: &Path, landed: Option<Seen>) -> PathBuf {
    let plain = dir.join(rel);
    let Ok(here) = std::fs::metadata(&plain) else {
        return plain;
    };
    let untouched = landed.is_some_and(|l| {
        l.size == here.len() && l.here_ms.is_none_or(|t| Some(t) == time_here(&here))
    });
    if untouched {
        plain
    } else {
        // There, and not what came last time (or never came from here): keep it.
        free_name(&plain)
    }
}

/// A file's last change here, in Unix ms.
fn time_here(meta: &std::fs::Metadata) -> Option<u64> {
    let since = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH);
    Some(since.ok()?.as_millis() as u64)
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
        let list = |dir: String, offset| async move { self.files_list(root, &dir, offset).await };
        walk(list, MAX_FILES).await
    }

    /// [`Machine::sync_look`] of the shared folder shown as `folder` (`Folder.path`), at
    /// `index` in `Overview.shared` now, with the root to get its files by: its id, so
    /// unsharing another folder during the round doesn't move it, or on agents from before
    /// ids, which can't read one, its place.
    pub async fn sync_look_shared(
        &self,
        folder: &str,
        index: u32,
    ) -> Result<(Root, Scan), ClientError> {
        let id = Root::SharedId(Root::shared_id(folder));
        let mut first = match self.files_list(id, "", 0).await {
            Err(ClientError::Remote(e)) if e.code == ErrorCode::Invalid => {
                let place = Root::Shared(index);
                return Ok((place, self.sync_look(place).await?));
            }
            first => Some(first?),
        };
        let list = |dir: String, offset| {
            let ready = if dir.is_empty() && offset == 0 {
                first.take()
            } else {
                None
            };
            async move {
                match ready {
                    Some(page) => Ok(page),
                    None => self.files_list(id, &dir, offset).await,
                }
            }
        };
        Ok((id, walk(list, MAX_FILES).await?))
    }

    /// Gets one file into `dir`, carrying on from a partial copy, and returns where it
    /// landed and how it came, to keep in [`Known`]: the machine's size and time as it was
    /// sent, which may be newer than the listing's. `landed`: how it came last time, if it did.
    pub async fn sync_get(
        &self,
        root: Root,
        file: &Remote,
        dir: &Path,
        landed: Option<Seen>,
        progress: impl FnMut(u64, u64),
    ) -> Result<(PathBuf, Seen), ClientError> {
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
        // Its time here after the mark, so the next version sees whether it was edited here.
        let here_ms = std::fs::metadata(&dest).ok().as_ref().and_then(time_here);
        let seen = Seen {
            size: got.size,
            modified_ms: got.modified_ms,
            here_ms,
        };
        Ok((dest, seen))
    }
}

/// Lists a folder tree page by page with `list(folder, offset)`, at most `max` files and
/// [`MAX_DEPTH`] deep. The agent lists folders newest first and the newest is looked in
/// first, so in a tree of more than `max` files the oldest are the ones left out. An entry
/// that comes again on the next page (the folder changed between pages) is taken once.
async fn walk<F, Fut>(mut list: F, max: usize) -> Result<Scan, ClientError>
where
    F: FnMut(String, u32) -> Fut,
    Fut: Future<Output = Result<(Vec<Entry>, Option<u32>), ClientError>>,
{
    let mut scan = Scan::default();
    let mut taken = HashSet::new();
    let mut folders = vec![(String::new(), 0usize)];
    while let Some((dir, depth)) = folders.pop() {
        let mut inside = Vec::new();
        let mut offset = 0;
        loop {
            let (entries, next) = list(dir.clone(), offset).await?;
            for e in entries {
                let path = if dir.is_empty() {
                    e.name
                } else {
                    format!("{dir}/{}", e.name)
                };
                if !taken.insert(path.clone()) {
                    continue;
                }
                match e.kind {
                    Kind::Folder if depth + 1 < MAX_DEPTH => inside.push((path, depth + 1)),
                    Kind::Folder => scan.deep += 1,
                    Kind::File => scan.files.push(Remote {
                        path,
                        size: e.size,
                        modified_ms: e.modified_ms,
                    }),
                }
                if scan.files.len() >= max {
                    scan.full = true;
                    return Ok(scan);
                }
            }
            match next {
                Some(n) => offset = n,
                None => break,
            }
        }
        // Newest on top, from every page of this folder.
        folders.extend(inside.into_iter().rev());
    }
    Ok(scan)
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
