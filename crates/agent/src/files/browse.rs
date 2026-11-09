//! Listing folders for devices (F40 user story 1). Pages hold folders first, then the
//! newest files; hidden names stay out of shared folders and the Inbox.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pocketgpu_proto::files::{Entry, Kind, PAGE, Response, Root};
use pocketgpu_proto::{Error, ErrorCode};

use super::offers;
use super::resolve::{Refused, Rules};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

/// Entries read from one folder at most.
const MOST_ENTRIES: usize = 100_000;

pub(super) fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// Where a root is on disk and how paths in it are checked.
pub(super) fn rules(agent: &Agent, root: Root) -> Result<Rules, Error> {
    fn not_found(what: &'static str) -> impl Fn(Refused) -> Error {
        move |_| error(ErrorCode::NotFound, what)
    }
    match root {
        Root::Shared(i) => {
            let dir = agent
                .config
                .read()
                .expect("config lock poisoned")
                .files_shared
                .get(i as usize)
                .cloned()
                .ok_or_else(|| error(ErrorCode::NotFound, "that folder is no longer shared"))?;
            Rules::new(&dir).map_err(not_found("that shared folder is gone"))
        }
        Root::Inbox => {
            Rules::new(&super::inbox_dir(agent)).map_err(not_found("the Inbox is empty"))
        }
        Root::JobLogs => {
            Rules::new(&agent.paths.jobs_dir()).map_err(not_found("there are no job logs"))
        }
        Root::Home => {
            if let Err(why) = super::terminal_on(agent) {
                return Err(error(ErrorCode::Forbidden, why));
            }
            let home = super::home()
                .ok_or_else(|| error(ErrorCode::NotFound, "this user has no home folder"))?;
            let mut rules = Rules::new(&home).map_err(not_found("the home folder is gone"))?;
            rules.hidden = true;
            // Everything the agent's user can read: links in the home folder may lead to
            // other disks; PocketGPU's own folders stay closed.
            rules.outside = true;
            rules.deny = super::guard::Context::of(agent).agent_dirs;
            Ok(rules)
        }
        Root::Offers => Err(error(ErrorCode::Invalid, "offers are opened by their id")),
    }
}

/// `12.log`: the log of job 12.
pub fn is_job_log(name: &str) -> bool {
    name.strip_suffix(".log")
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

/// Job log files in the jobs folder.
pub fn job_logs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .take(MOST_ENTRIES)
        .filter(|e| is_job_log(&e.file_name().to_string_lossy()))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect()
}

pub async fn list(
    agent: &Arc<Agent>,
    device: &str,
    root: Root,
    path: String,
    offset: u32,
) -> Response {
    let (agent, device) = (agent.clone(), device.to_string());
    let listed = tokio::task::spawn_blocking(move || entries(&agent, &device, root, &path)).await;
    match listed {
        Ok(Ok(all)) => page(all, offset),
        Ok(Err(e)) => Response::Error(e),
        Err(e) => Response::Error(error(ErrorCode::Internal, e.to_string())),
    }
}

fn entries(agent: &Agent, device: &str, root: Root, path: &str) -> Result<Vec<Entry>, Error> {
    match root {
        Root::Offers => {
            if !path.is_empty() {
                return Err(error(ErrorCode::NotFound, "offers hold no folders"));
            }
            let list = offers::for_device(&agent.db(), device, now_ms())
                .map_err(|e| error(ErrorCode::Internal, e.to_string()))?;
            // An offer is saved by its id, so that is its name here; Overview has the
            // file names.
            Ok(list
                .into_iter()
                .map(|o| Entry {
                    name: o.id,
                    kind: Kind::File,
                    size: o.size,
                    modified_ms: o.modified_ms,
                })
                .collect())
        }
        Root::JobLogs => {
            if !path.is_empty() {
                return Err(error(ErrorCode::NotFound, "job logs hold no folders"));
            }
            Ok(job_logs(&agent.paths.jobs_dir())
                .iter()
                .filter_map(|p| file_entry(p))
                .collect())
        }
        // No Inbox yet: nothing was sent.
        Root::Inbox if path.is_empty() && !super::inbox_dir(agent).exists() => Ok(Vec::new()),
        _ => {
            let rules = rules(agent, root)?;
            let dir = rules.folder(path).map_err(|e| e.to_error())?;
            Ok(read_folder(&rules, &dir))
        }
    }
}

fn file_entry(path: &Path) -> Option<Entry> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Entry {
        name: path.file_name()?.to_string_lossy().into_owned(),
        kind: Kind::File,
        size: meta.len(),
        modified_ms: super::modified_ms(&meta),
    })
}

/// Files and folders in a checked folder: hidden ones only where the root allows them,
/// links only when they stay inside, no devices, pipes or sockets.
fn read_folder(rules: &Rules, dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .take(MOST_ENTRIES)
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if !rules.hidden && name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_symlink() {
            let inside = path.canonicalize().is_ok_and(|c| rules.check(&c).is_ok());
            if !inside {
                continue;
            }
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let kind = if meta.is_dir() {
            Kind::Folder
        } else if meta.is_file() {
            Kind::File
        } else {
            continue;
        };
        out.push(Entry {
            name,
            kind,
            size: if kind == Kind::File { meta.len() } else { 0 },
            modified_ms: super::modified_ms(&meta),
        });
    }
    out
}

/// Folders first, then newest first, then by name; one page from `offset`.
pub fn page(mut all: Vec<Entry>, offset: u32) -> Response {
    all.sort_by(|a, b| {
        (b.kind == Kind::Folder)
            .cmp(&(a.kind == Kind::Folder))
            .then(b.modified_ms.cmp(&a.modified_ms))
            .then_with(|| a.name.cmp(&b.name))
    });
    let start = (offset as usize).min(all.len());
    let end = start.saturating_add(PAGE as usize).min(all.len());
    let next = (end < all.len()).then_some(end as u32);
    Response::Entries {
        entries: all.drain(start..end).collect(),
        next,
    }
}

#[cfg(test)]
#[path = "browse_tests.rs"]
mod tests;
