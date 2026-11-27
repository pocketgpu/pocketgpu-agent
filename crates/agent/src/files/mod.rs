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
pub mod secrets;
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

/// Folder reads still going. On a Mac, opening Documents, Downloads or Desktop waits for the
/// person to allow it (a dialog that may sit unseen on the screen), and a plain read would wait
/// with it, once more for every request. A read in this set is not started again until it
/// comes back.
static READING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
    std::sync::LazyLock::new(Default::default);

/// [`count_files`] that gives up after `wait`: `None` while the folder won't open yet (macOS
/// asking for permission, a network drive that doesn't answer), so a request for the overview
/// still answers with everything else. The read carries on in its own thread and, once it
/// comes back, later requests count normally.
pub async fn count_files_within(dir: PathBuf, wait: Duration) -> Option<u32> {
    let read = dir.clone();
    within(dir, wait, move || count_files(&read)).await
}

/// Runs the blocking `read` of `key` in its own thread for up to `wait`: `None` when it took
/// longer, or when a read of the same `key` is still going (no second one starts then).
pub(crate) async fn within<T: Send + 'static>(
    key: PathBuf,
    wait: Duration,
    read: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    if !READING.lock().expect("reading lock").insert(key.clone()) {
        return None;
    }
    let task = tokio::task::spawn_blocking(move || {
        let out = read();
        READING.lock().expect("reading lock").remove(&key);
        out
    });
    tokio::time::timeout(wait, task).await.ok()?.ok()
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

#[cfg(test)]
mod count_tests {
    use super::*;

    #[tokio::test]
    async fn counts_a_folder_that_opens() {
        let dir = PathBuf::from("/tmp/pgpu-count-ok");
        let n = within(dir, Duration::from_secs(2), || 7).await;
        assert_eq!(n, Some(7));
    }

    #[tokio::test]
    async fn gives_up_on_a_folder_that_waits_and_does_not_pile_up_reads() {
        let dir = PathBuf::from("/tmp/pgpu-count-slow");
        let slow = || {
            std::thread::sleep(Duration::from_millis(400));
            3
        };
        // The first read waits longer than we do.
        assert_eq!(
            within(dir.clone(), Duration::from_millis(50), slow).await,
            None
        );
        // Asked again while it is still waiting: no second read starts.
        let started = std::time::Instant::now();
        assert_eq!(
            within(dir.clone(), Duration::from_secs(2), || 99).await,
            None
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        // Once the first read is over, counting works again.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(within(dir, Duration::from_secs(2), || 5).await, Some(5));
    }
}
