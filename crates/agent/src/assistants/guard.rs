//! The guard file (F32, research §4): `assistants.guard` in the agent's data folder (where
//! its fallback socket lives too) says `armed` while any "Ask me first" kind or own word is
//! on. When the hook can't reach the agent (stopped, restarting, too old), `pgpu hook
//! --check` reads it: armed gives the fail-closed answer, absent lets the call go on. It is
//! kept in the data folder, not the runtime one, so it survives a reboot before the agent
//! starts again.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::store::paths::Paths;

pub const FILE: &str = "assistants.guard";
const ARMED: &str = "armed";

pub fn path(paths: &Paths) -> PathBuf {
    paths.data_dir.join(FILE)
}

/// Writes `armed`, or removes the file. Called on every settings change and at start.
pub fn write(paths: &Paths, armed: bool) -> Result<()> {
    let path = path(paths);
    if armed {
        crate::store::paths::write_private(&path, format!("{ARMED}\n").as_bytes())
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("could not remove {}", path.display())),
        }
    }
}

/// Whether the guard says armed: any file there does, even one that can't be read or says
/// something else. The safe side is to ask, never to let a held kind through.
pub fn armed(paths: &Paths) -> bool {
    match std::fs::metadata(path(paths)) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_read_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path().to_path_buf());
        assert!(!armed(&paths));
        write(&paths, true).unwrap();
        assert!(armed(&paths));
        write(&paths, true).unwrap();
        assert!(armed(&paths));
        write(&paths, false).unwrap();
        assert!(!armed(&paths));
        write(&paths, false).unwrap();
        // Something odd there still counts as armed.
        std::fs::write(path(&paths), "?").unwrap();
        assert!(armed(&paths));
    }
}
