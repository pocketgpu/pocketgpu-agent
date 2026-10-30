//! Where the agent keeps its files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    /// Platform paths, or everything under `PGPU_DATA_DIR` (used in containers: `/data`).
    pub fn discover() -> Result<Self> {
        if let Some(dir) = std::env::var_os("PGPU_DATA_DIR") {
            return Ok(Self::under(PathBuf::from(dir)));
        }
        let dirs = directories::ProjectDirs::from("app", "pocketgpu", "pocketgpu")
            .context("could not find a home directory")?;
        let runtime_dir = dirs
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_local_dir().to_path_buf(),
            runtime_dir,
        })
    }

    /// All paths under one directory: containers and tests.
    pub fn under(root: PathBuf) -> Self {
        Self {
            config_dir: root.clone(),
            data_dir: root.clone(),
            runtime_dir: root,
        }
    }

    pub fn create_all(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.runtime_dir,
            &self.jobs_dir(),
        ] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
            // Best effort: a container's mounted volume may belong to someone else.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn key_file(&self) -> PathBuf {
        self.data_dir.join("agent.key")
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("pocketgpu.db")
    }

    pub fn jobs_dir(&self) -> PathBuf {
        self.data_dir.join("jobs")
    }

    /// Output of the background agent on Windows; systemd keeps it in the journal on Linux.
    pub fn agent_log(&self) -> PathBuf {
        self.data_dir.join("agent.log")
    }

    /// Local socket for the command line (a named pipe name on Windows).
    pub fn socket(&self) -> PathBuf {
        self.runtime_dir.join("pocketgpu.sock")
    }
}

/// Logs larger than this are cut back at start: Windows and macOS have no journal to do it.
const MAX_LOG: u64 = 10 * 1024 * 1024;

/// Keeps a log file the agent appends to from growing without end: past [`MAX_LOG`] the
/// old content moves to `<name>.1` (replacing an older one), or, when the file is held open
/// by a service manager, is emptied in place.
pub fn rotate_log(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG) {
        let old = path.with_extension("log.1");
        if std::fs::rename(path, &old).is_err()
            && let Ok(f) = std::fs::OpenOptions::new().write(true).open(path)
        {
            let _ = f.set_len(0);
        }
    }
}

/// Writes a file readable only by the owner (mode 0600 on Unix). The bytes go to a new file
/// that is private from its creation and then replaces `path`, so the file is never readable
/// by others, nor half written.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("could not write {}", path.display()));
    }
    set_private(path)
}

#[cfg(unix)]
pub fn set_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict {}", path.display()))
}

#[cfg(not(unix))]
pub fn set_private(_path: &Path) -> Result<()> {
    // Files under the user profile are private to the user by default on Windows.
    Ok(())
}
