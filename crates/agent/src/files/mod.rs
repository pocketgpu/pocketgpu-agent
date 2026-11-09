//! Files between paired devices and this machine (F40): folders shared with
//! `pgpu files share`, the Inbox that sent files land in, files offered with `pgpu send`,
//! job logs, and for Shell devices while the terminal is on, everything the agent's user
//! can read. Off until `pgpu files on` at the machine; phones can't turn it on.

mod browse;
mod get;
pub mod guard;
pub mod handler;
pub mod inbox;
mod local;
pub mod offers;
mod place;
pub mod resolve;
pub mod room;
mod still;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use local::local_request;

use crate::daemon::state::Agent;
use crate::store::paths::Paths;

/// The user's home folder, canonical.
pub fn home() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(home.canonicalize().unwrap_or(home))
}

/// "All your files" is open only while the terminal could open: switched on with
/// `pgpu shell on`, and not as root in a container (where the terminal stays off too).
pub fn terminal_on(agent: &Agent) -> Result<(), &'static str> {
    if !agent.config.read().expect("config lock poisoned").shell {
        return Err(
            "all your files needs the terminal on at the machine. Turn it on there \
                    with: pgpu shell on",
        );
    }
    if matches!(
        crate::daemon::root::check(),
        Ok(crate::daemon::root::RootCheck::RootInContainer)
    ) {
        return Err("the agent runs as root in this container, so all your files stays off");
    }
    Ok(())
}

/// Where sent files land: the folder set with `pgpu files inbox`, else the default place.
pub fn inbox_dir(agent: &Agent) -> PathBuf {
    let set = agent
        .config
        .read()
        .expect("config lock poisoned")
        .files_inbox
        .clone();
    set.unwrap_or_else(|| default_inbox(&agent.paths))
}

/// `<data>/inbox` in a container; `Downloads/PocketGPU/Inbox` on Windows and macOS;
/// `~/PocketGPU/Inbox` elsewhere, or when there is no Downloads folder.
fn default_inbox(paths: &Paths) -> PathBuf {
    if crate::daemon::root::in_container() {
        return paths.data_dir.join("inbox");
    }
    let Some(user) = directories::UserDirs::new() else {
        return paths.data_dir.join("inbox");
    };
    let base = match user.download_dir() {
        Some(downloads) if cfg!(any(windows, target_os = "macos")) => downloads.to_path_buf(),
        _ => user.home_dir().to_path_buf(),
    };
    base.join("PocketGPU").join("Inbox")
}

/// The Inbox, created (private to this user) when it doesn't exist yet.
pub fn ensure_inbox(agent: &Agent) -> std::io::Result<PathBuf> {
    let dir = inbox_dir(agent);
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir)?;
        private_dir(&dir);
    }
    Ok(dir)
}

/// Mode 0700 on Unix; folders under the user's profile are private already on Windows.
pub fn private_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// A path as people know it: `~/ComfyUI/output`, or on Windows `C:\Users\ana\Downloads`
/// without the `\\?\` that canonical paths start with.
pub fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    if cfg!(windows) {
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        return text.strip_prefix(r"\\?\").unwrap_or(&text).to_string();
    }
    match home().and_then(|h| path.strip_prefix(&h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => text,
    }
}

/// Unix milliseconds of a file's last change, 0 when the system doesn't say.
pub fn modified_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

/// Regular files directly in a folder, hidden ones left out; 0 when it can't be read.
pub fn count_files(dir: &Path) -> u32 {
    const MOST: usize = 100_000;
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .take(MOST)
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .count() as u32
    })
}

/// At start and then hourly: unfinished sends older than a day and expired offers go.
pub async fn tidy_hourly(agent: Arc<Agent>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    loop {
        tick.tick().await;
        let dir = inbox_dir(&agent);
        let removed = tokio::task::spawn_blocking(move || {
            inbox::remove_old_partials(&dir, std::time::SystemTime::now())
        })
        .await
        .unwrap_or(0);
        if removed > 0 {
            tracing::info!(removed, "removed unfinished sends older than a day");
        }
        if let Err(e) = offers::prune(&agent.db(), crate::store::db::now_ms()) {
            tracing::warn!("could not remove expired offers: {e:#}");
        }
    }
}
