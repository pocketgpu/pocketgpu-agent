//! Listing folders for devices (F40 user story 1). Pages hold folders first, then the
//! newest files; hidden names stay out of shared folders and the Inbox.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pocketgpu_proto::files::{Entry, Kind, PAGE, Response, Root};
use pocketgpu_proto::{Error, ErrorCode};

use super::offers;
use super::resolve::{self, FileId, Refused, Rules};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

/// Entries read from one folder at most.
const MOST_ENTRIES: usize = 100_000;

pub(super) fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// [`root_rules`] with what this device may not open in it.
pub(super) fn rules(agent: &Agent, root: Root, closed: &Closed) -> Result<Rules, Error> {
    let mut rules = root_rules(agent, root)?;
    rules.deny.extend(closed.paths.iter().cloned());
    rules.deny_ids.extend(&closed.ids);
    Ok(rules)
}

/// Folders a device may not open: for a guest (F44), the Inbox, which holds what others
/// sent, also inside a shared folder (`Downloads` holds it). Empty for everyone else.
#[derive(Debug, Clone, Default)]
pub(super) struct Closed {
    paths: Vec<PathBuf>,
    ids: Vec<FileId>,
}

impl Closed {
    /// Whether a canonical path is in a closed folder.
    pub(super) fn holds(&self, canon: &Path) -> bool {
        resolve::denied(canon, &self.paths, &self.ids)
    }
}

/// The Inbox, closed to a guest: as set, and as last found on disk.
pub(super) async fn inbox_closed(agent: &Agent) -> Closed {
    let dir = super::inbox_dir(agent);
    let found = INBOX.find(dir.clone(), look_at).await;
    let mut closed = Closed {
        paths: vec![dir],
        ids: Vec::new(),
    };
    if let Some(Some((canon, id))) = found {
        closed.paths.push(canon);
        closed.ids.extend(id);
    }
    closed
}

/// A folder's canonical path and which folder it is on disk; `None` when it isn't there.
type Found = Option<(PathBuf, Option<FileId>)>;

fn look_at(dir: &Path) -> Found {
    let canon = dir.canonicalize().ok()?;
    let id = std::fs::metadata(&canon).ok().and_then(|m| FileId::of(&m));
    Some((canon, id))
}

static INBOX: Looker = Looker::new();

/// How long a request waits for the Inbox to be looked at.
const LOOK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// Looks at one folder on disk for requests. On a Mac that can wait for the person to allow
/// Downloads (a dialog that may sit unseen), so a request waits only [`LOOK_WAIT`], at most
/// one look is going, and while one takes long the last answer stands in.
struct Looker(tokio::sync::Mutex<Option<Look>>);

struct Look {
    dir: PathBuf,
    found: Option<Found>,
    going: Option<tokio::task::JoinHandle<Found>>,
}

impl Looker {
    const fn new() -> Self {
        Self(tokio::sync::Mutex::const_new(None))
    }

    /// What `look` finds at `dir` now, or the last answer while a look takes long; `None`
    /// when there is none yet. Requests at the same time take turns, so none goes without
    /// an answer just because another one is looking.
    async fn find(&self, dir: PathBuf, look: fn(&Path) -> Found) -> Option<Found> {
        let mut state = self.0.lock().await;
        if state.as_ref().is_none_or(|l| l.dir != dir) {
            *state = Some(Look {
                dir: dir.clone(),
                found: None,
                going: None,
            });
        }
        let state = state.as_mut().expect("set above");
        // A look that already took long isn't waited for again.
        let wait = if state.going.is_some() {
            std::time::Duration::ZERO
        } else {
            LOOK_WAIT
        };
        let going = state
            .going
            .get_or_insert_with(|| tokio::task::spawn_blocking(move || look(&dir)));
        if let Ok(done) = tokio::time::timeout(wait, going).await {
            state.going = None;
            if let Ok(found) = done {
                state.found = Some(found);
            }
        }
        state.found.clone()
    }
}

/// Where a root is on disk and how paths in it are checked.
fn root_rules(agent: &Agent, root: Root) -> Result<Rules, Error> {
    fn not_found(what: &'static str) -> impl Fn(Refused) -> Error {
        move |_| error(ErrorCode::NotFound, what)
    }
    match root {
        Root::Shared(_) | Root::SharedId(_) => {
            let shared = agent
                .config
                .read()
                .expect("config lock poisoned")
                .files_shared
                .clone();
            let dir = match root {
                // By how it is shown, as in `Overview`: unsharing another doesn't move it.
                Root::SharedId(id) => shared
                    .iter()
                    .find(|p| Root::shared_id(&super::shown(p)) == id),
                Root::Shared(i) => shared.get(i as usize),
                _ => None,
            }
            .ok_or_else(|| error(ErrorCode::NotFound, "that folder is no longer shared"))?;
            Rules::new(dir).map_err(not_found("that shared folder is gone"))
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
            rules.deny = super::guard::context(agent).agent_dirs;
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
    closed: Closed,
) -> Response {
    let (agent, device) = (agent.clone(), device.to_string());
    let key = PathBuf::from(format!("list {root:?}")).join(&path);
    let read = move || entries(&agent, &device, root, &path, &closed);
    match super::within(key, LIST_WAIT, read).await {
        Some(Ok(all)) => page(all, offset),
        Some(Err(e)) => Response::Error(e),
        None => Response::Error(error(ErrorCode::Busy, NOT_OPENING)),
    }
}

/// How long a folder may take to open before the device is told why it waits.
const LIST_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Said when a folder doesn't open in time: on a Mac, nearly always the permission dialog.
const NOT_OPENING: &str = if cfg!(target_os = "macos") {
    "This folder isn't opening yet: macOS is asking for permission on this machine. Click \
     Allow there, or turn on pocketgpu in System Settings → Privacy & Security → Files and \
     Folders, then try again"
} else {
    "This folder isn't opening yet on this machine (a slow or network drive?). Try again in \
     a moment"
};

fn entries(
    agent: &Agent,
    device: &str,
    root: Root,
    path: &str,
    closed: &Closed,
) -> Result<Vec<Entry>, Error> {
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
            let rules = rules(agent, root, closed)?;
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
/// links only when they stay inside, nothing in a closed folder, no devices, pipes or
/// sockets.
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
        } else if rules.deny.iter().any(|d| path.starts_with(d)) {
            // `dir` is canonical, so `path` is too.
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        // The folders above it were checked with `dir`.
        if FileId::of(&meta).is_some_and(|id| rules.deny_ids.contains(&id)) {
            continue;
        }
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
