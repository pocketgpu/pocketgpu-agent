//! Synced folders (F43, board 56): one of a machine's shared folders, kept on this device.
//! One way, from the machine to here; nothing here is ever deleted. Each round lists the
//! folder (and its folders), works out what is new or changed since the last round
//! ([`Known`]), and downloads each such file into `.pgpu-partial/` first, so a dropped
//! connection carries on where it stopped and a half file is never in the folder. Every file
//! is checked against the machine's BLAKE3 hash before it is moved into place. A file that
//! was changed here since it came is kept: the new one gets a `(2)` name next to it.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use pocketgpu_proto::files::{Kind, Root};
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

/// `a/b.txt` as a relative path, or `None` for anything that could leave the folder or
/// lands in PocketGPU's own partial folder.
pub fn safe(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(n) => out.push(n),
            _ => return None,
        }
    }
    let first = out.components().next()?;
    (first.as_os_str() != PARTIAL).then_some(out)
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
        let mut out = Vec::new();
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
                        Kind::Folder => {}
                        _ => out.push(Remote {
                            path,
                            size: e.size,
                            modified_ms: e.modified_ms,
                        }),
                    }
                    if out.len() >= MAX_FILES {
                        return Ok(out);
                    }
                }
                match next {
                    Some(n) => offset = n,
                    None => break,
                }
            }
        }
        Ok(out)
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
        let partial = dir.join(PARTIAL).join(&rel);
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
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ClientError::File)?;
        }
        tokio::fs::rename(&partial, &dest)
            .await
            .map_err(ClientError::File)?;
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
        let remote = [r("../x", 1, 1)];
        assert!(needed(&remote, &Known::default()).is_empty());
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
