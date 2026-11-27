//! Writing an assistant's files carefully: whole or not at all, with their old permissions,
//! through a link to where the file really is (dotfile managers link settings files).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// The file's text, or `None` when there is no file.
pub fn read(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .with_context(|| format!("{} isn't text", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

/// Replaces the file's text in one step, keeping its permissions; makes it when missing (only
/// its owner may read a new one).
pub fn write(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let perms = std::fs::metadata(&target).ok().map(|m| m.permissions());
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.pgpu-{}", std::process::id()));
    // Settings files often hold API keys in an MCP server's `env`, and the folder may be
    // open to others: the copy is private from the start, never readable while it fills.
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .and_then(|()| match &perms {
            Some(p) => std::fs::set_permissions(&tmp, p.clone()),
            None => Ok(()),
        })
        .and_then(|()| std::fs::rename(&tmp, &target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("could not write {}", path.display()));
    }
    Ok(())
}

/// Makes the folders up to `dir`, and says which ones were new (outermost first), so taking
/// the change back can remove them too.
pub fn make_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut missing = Vec::new();
    let mut at = Some(dir);
    while let Some(d) = at {
        if d.exists() {
            break;
        }
        missing.push(d.to_path_buf());
        at = d.parent();
    }
    missing.reverse();
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    Ok(missing)
}

/// Removes the folders that were made for a change, innermost first, when they are empty.
pub fn remove_dirs(dirs: &[PathBuf]) {
    for d in dirs.iter().rev() {
        let _ = std::fs::remove_dir(d);
    }
}

/// What a file held, to tell whether someone changed it since.
pub fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn new_files_are_private_and_old_ones_keep_their_mode() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("mcp.json");
        write(&new, "{}").unwrap();
        assert_eq!(mode(&new), 0o600);
        assert_eq!(read(&new).unwrap().as_deref(), Some("{}"));

        let old = dir.path().join("settings.json");
        std::fs::write(&old, "a").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o640)).unwrap();
        // A copy left over from a run that was killed doesn't stop the next one.
        let stale = dir
            .path()
            .join(format!(".settings.json.pgpu-{}", std::process::id()));
        std::fs::write(&stale, "stale").unwrap();
        write(&old, "b").unwrap();
        assert_eq!(mode(&old), 0o640);
        assert_eq!(read(&old).unwrap().as_deref(), Some("b"));
        assert!(!stale.exists());
    }
}
