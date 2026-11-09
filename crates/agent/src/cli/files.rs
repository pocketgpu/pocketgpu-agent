//! `pgpu files [status | on | off | share <folders>… | unshare <folder> | inbox [<folder>]]`:
//! files for other devices (F40). Works only at the machine: phones can't turn it on or
//! share a folder.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{FilesStatus, Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show whether files are on, the shared folders and the Inbox (the default).
    Status,
    /// Let phones that can run actions use shared folders and the Inbox.
    On,
    /// Stop every device from seeing or sending files here.
    Off,
    /// Share folders with phones that can run actions: pgpu files share ~/ComfyUI/output
    Share {
        #[arg(required = true, value_name = "FOLDER")]
        folders: Vec<PathBuf>,
    },
    /// Stop sharing a folder.
    Unshare {
        #[arg(value_name = "FOLDER")]
        folder: PathBuf,
    },
    /// Show where sent files land, or move the Inbox to another folder.
    Inbox {
        #[arg(value_name = "FOLDER")]
        folder: Option<PathBuf>,
    },
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let action = action.unwrap_or(Action::Status);
    let status = match &action {
        Action::Status | Action::Inbox { folder: None } => call(paths, Request::GetFiles).await?,
        Action::On => call(paths, Request::SetFiles { on: true }).await?,
        Action::Off => call(paths, Request::SetFiles { on: false }).await?,
        Action::Share { folders } => {
            let mut last = None;
            for folder in folders {
                let path = absolute(folder)?;
                let status = call(paths, Request::ShareFolder { path })
                    .await
                    .with_context(|| format!("could not share {}", folder.display()))?;
                last = Some(status);
            }
            last.expect("clap requires a folder")
        }
        Action::Unshare { folder } => {
            // A folder deleted since it was shared can still be unshared by its old path.
            let path = match absolute(folder) {
                Ok(p) => p,
                Err(_) => text(&std::path::absolute(folder)?)?,
            };
            call(paths, Request::UnshareFolder { path }).await?
        }
        Action::Inbox {
            folder: Some(folder),
        } => {
            let path = absolute(folder)?;
            call(paths, Request::SetInbox { path }).await?
        }
    };
    if json {
        println!("{}", serde_json::to_string(&status)?);
    } else if let Action::Inbox { folder } = &action {
        print!("{}", render_inbox(&status, folder.is_some()));
    } else {
        print!("{}", render(&status));
    }
    Ok(())
}

async fn call(paths: &Paths, req: Request) -> Result<FilesStatus> {
    match client::call(paths, req).await? {
        Response::Files(s) => Ok(s),
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
}

/// The path as the agent needs it: absolute, links resolved, and existing.
pub fn absolute(path: &Path) -> Result<String> {
    let full = std::fs::canonicalize(path)
        .with_context(|| format!("{} doesn't exist or can't be opened", path.display()))?;
    text(&full)
}

fn text(path: &Path) -> Result<String> {
    match path.to_str() {
        Some(s) => Ok(s.to_string()),
        None => bail!(
            "{}: PocketGPU can't use a path that isn't valid Unicode",
            path.display()
        ),
    }
}

pub fn render(s: &FilesStatus) -> String {
    let mut out = if s.enabled {
        "Files are on. Phones that can run actions can use shared folders and the Inbox.\n"
            .to_string()
    } else {
        "Files are off: no device can see or send files here.\n".to_string()
    };
    if s.shared.is_empty() {
        out.push_str("No folders are shared. Share one with: pgpu files share <folder>\n");
    } else {
        out.push_str("Shared folders:\n");
        for f in &s.shared {
            out.push_str(&format!("  {f}\n"));
        }
    }
    out.push_str(&format!(
        "Inbox: {} ({})\n",
        s.inbox,
        count(s.inbox_files, "file")
    ));
    if s.offers > 0 {
        out.push_str(&format!(
            "Offered with pgpu send: {}\n",
            count(s.offers, "file")
        ));
    }
    out.push_str(if s.enabled {
        "Turn off with: pgpu files off\n"
    } else {
        "Turn on with: pgpu files on\n"
    });
    out
}

pub fn render_inbox(s: &FilesStatus, moved: bool) -> String {
    let what = if moved {
        "Files sent here now land in"
    } else {
        "Files sent here land in"
    };
    format!(
        "{what} {} ({} there).\nSee them with: pgpu inbox · Move it with: pgpu files inbox <folder>\n",
        s.inbox,
        count(s.inbox_files, "file")
    )
}

/// `1 file`, `3 files`.
pub fn count(n: u32, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `812 bytes`, `4.2 KB`, `320 MB`, `1.5 GB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return if bytes == 1 {
            "1 byte".into()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(enabled: bool, shared: &[&str]) -> FilesStatus {
        FilesStatus {
            enabled,
            shared: shared.iter().map(|s| s.to_string()).collect(),
            inbox: "~/PocketGPU/Inbox".into(),
            inbox_files: 3,
            offers: 1,
        }
    }

    #[test]
    fn says_what_is_shared_and_how_to_change_it() {
        let on = render(&status(true, &["~/ComfyUI/output", "~/train/out"]));
        assert!(
            on.starts_with(
                "Files are on. Phones that can run actions can use shared folders and the Inbox.\n"
            ),
            "{on}"
        );
        assert!(on.contains("  ~/ComfyUI/output\n  ~/train/out\n"), "{on}");
        assert!(on.contains("Inbox: ~/PocketGPU/Inbox (3 files)"), "{on}");
        assert!(on.contains("Offered with pgpu send: 1 file"), "{on}");
        assert!(on.ends_with("Turn off with: pgpu files off\n"), "{on}");

        let off = render(&status(false, &[]));
        assert!(off.contains("pgpu files share <folder>"), "{off}");
        assert!(off.ends_with("Turn on with: pgpu files on\n"), "{off}");
    }

    #[test]
    fn the_inbox_says_where_files_land() {
        let s = status(true, &[]);
        assert!(render_inbox(&s, false).starts_with("Files sent here land in ~/PocketGPU/Inbox"));
        assert!(render_inbox(&s, true).starts_with("Files sent here now land in"));
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(size(0), "0 bytes");
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(812), "812 bytes");
        assert_eq!(size(4300), "4.2 KB");
        assert_eq!(size(320 << 20), "320 MB");
        assert_eq!(size(3 << 29), "1.5 GB");
        assert_eq!(size(u64::MAX), "16777216 TB");
    }

    #[test]
    fn paths_are_made_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let abs = absolute(dir.path()).unwrap();
        assert!(Path::new(&abs).is_absolute());
        assert!(absolute(&dir.path().join("missing")).is_err());
    }
}
