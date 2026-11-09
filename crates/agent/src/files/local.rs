//! The command line's files requests (`pgpu files`, `pgpu inbox`, `pgpu send`): switches
//! and folders saved in `config.toml` and audited, the Inbox's files, and offers. Only
//! here, at the machine: no phone request changes any of it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pocketgpu_proto::Permission;
use pocketgpu_proto::files::clean_name;
use pocketgpu_proto::local::{FilesStatus, InboxFile, Request, Response};

use super::guard::{self, Context};
use super::{inbox_dir, offers, shown};
use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::config::Config;
use crate::store::db::now_ms;

/// Files listed by `pgpu inbox` at most, the newest.
const MOST_LISTED: usize = 1000;

/// `GetFiles`, `SetFiles`, `ShareFolder`, `UnshareFolder`, `SetInbox`, `Inbox` and
/// `Offer`. Paths arrive absolute.
pub async fn local_request(agent: &Arc<Agent>, req: Request) -> Response {
    let agent = agent.clone();
    tokio::task::spawn_blocking(move || handle(&agent, req))
        .await
        .unwrap_or_else(|e| Response::Error(e.to_string()))
}

fn handle(agent: &Arc<Agent>, req: Request) -> Response {
    let done = match req {
        Request::GetFiles => Ok(()),
        Request::SetFiles { on } => change(agent, |c| c.files = on).map(|()| {
            record(agent, "files", if on { "on" } else { "off" });
        }),
        Request::ShareFolder { path } => share(agent, Path::new(&path)),
        Request::UnshareFolder { path } => unshare(agent, &path),
        Request::SetInbox { path } => set_inbox(agent, Path::new(&path)),
        Request::Inbox => return Response::InboxList(inbox_files(agent)),
        Request::Offer { path, to } => {
            return offer(agent, Path::new(&path), to.as_deref()).unwrap_or_else(Response::Error);
        }
        _ => Err("not a files request".to_string()),
    };
    match done {
        Ok(()) => Response::Files(status(agent)),
        Err(e) => Response::Error(e),
    }
}

pub fn status(agent: &Agent) -> FilesStatus {
    let (enabled, shared) = {
        let cfg = agent.config.read().expect("config lock poisoned");
        (cfg.files, cfg.files_shared.clone())
    };
    let inbox = inbox_dir(agent);
    FilesStatus {
        enabled,
        shared: shared.iter().map(|p| shown(p)).collect(),
        inbox: shown(&inbox),
        inbox_files: super::count_files(&inbox),
        offers: offers::count(&agent.db(), now_ms()).unwrap_or(0),
    }
}

/// Saves a change to `config.toml`, then makes it current.
fn change(agent: &Agent, edit: impl FnOnce(&mut Config)) -> Result<(), String> {
    let mut cfg = agent.config.write().expect("config lock poisoned");
    let mut next = cfg.clone();
    edit(&mut next);
    next.save(&agent.paths.config_file())
        .map_err(|e| format!("{e:#}"))?;
    *cfg = next;
    Ok(())
}

fn record(agent: &Agent, target: &str, result: &str) {
    if let Err(e) = audit::record(&agent.db(), "local", Event::FilesAccess, target, result) {
        tracing::warn!("could not write audit entry: {e}");
    }
}

fn share(agent: &Agent, path: &Path) -> Result<(), String> {
    let canon = guard::shareable(path, &Context::of(agent))?;
    let added = canon.clone();
    let mut new = false;
    change(agent, |c| {
        if !c.files_shared.contains(&added) {
            c.files_shared.push(added);
            new = true;
        }
    })?;
    if new {
        record(agent, &format!("share {}", shown(&canon)), "ok");
    }
    Ok(())
}

/// By the path as shared, as it resolves now, or as `pgpu files` shows it.
fn unshare(agent: &Agent, path: &str) -> Result<(), String> {
    let given = PathBuf::from(path);
    let canon = given.canonicalize().ok();
    let shared = agent
        .config
        .read()
        .expect("config lock poisoned")
        .files_shared
        .clone();
    let Some(found) = shared
        .iter()
        .find(|s| **s == given || Some(*s) == canon.as_ref() || shown(s) == path)
        .cloned()
    else {
        return Err(format!(
            "{path} is not shared; `pgpu files` lists the shared folders"
        ));
    };
    change(agent, |c| c.files_shared.retain(|s| *s != found))?;
    record(agent, &format!("unshare {}", shown(&found)), "ok");
    Ok(())
}

/// Moves the Inbox. Unfinished sends in the old one go: devices start them over in the new
/// one, and nothing would ever remove them there.
fn set_inbox(agent: &Agent, path: &Path) -> Result<(), String> {
    let canon = guard::inbox(path, &Context::of(agent))?;
    let old = inbox_dir(agent);
    let target = format!("inbox {}", shown(&canon));
    // The default place isn't kept canonical (home may be a link, as on Fedora Atomic).
    let moved = old.canonicalize().unwrap_or_else(|_| old.clone()) != canon;
    change(agent, |c| c.files_inbox = Some(canon))?;
    record(agent, &target, "ok");
    if moved {
        let removed = super::inbox::remove_partials(&old);
        if removed > 0 {
            tracing::info!(removed, "removed unfinished sends from the old Inbox");
        }
    }
    Ok(())
}

/// Files in the Inbox, newest first, with the device that sent each (from the audit log,
/// so empty once that entry is gone).
fn inbox_files(agent: &Agent) -> Vec<InboxFile> {
    let dir = inbox_dir(agent);
    let mut files: Vec<(String, u64, i64)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
            (!name.starts_with('.')).then(|| (name, meta.len(), super::modified_ms(&meta) as i64))
        })
        .collect();
    files.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    files.truncate(MOST_LISTED);
    let senders = senders(agent);
    files
        .into_iter()
        .map(|(name, size, at_ms)| InboxFile {
            from: senders.get(&name).cloned().unwrap_or_default(),
            name,
            size,
            at_ms,
        })
        .collect()
}

/// The device that most recently sent each name into the Inbox.
fn senders(agent: &Agent) -> HashMap<String, String> {
    let db = agent.db();
    let Ok(mut stmt) = db.prepare(
        "SELECT a.target, coalesce(d.name, '') FROM audit a
         LEFT JOIN devices d ON d.device_id = a.device_id
         WHERE a.event = ?1 AND a.result = 'ok' ORDER BY a.at, a.audit_id",
    ) else {
        return HashMap::new();
    };
    let rows = stmt.query_map([Event::FilePut.as_str()], |r| Ok((r.get(0)?, r.get(1)?)));
    // Later entries replace earlier ones: the newest sender wins.
    rows.map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// `pgpu send`: offers a file for 7 days to devices with Control, or to the one named.
/// Refused while files are off, as no device could save it.
fn offer(agent: &Arc<Agent>, path: &Path, to: Option<&str>) -> Result<Response, String> {
    if !agent.config.read().expect("config lock poisoned").files {
        return Err(
            "files are off here, so no device could save it. Turn them on first with: \
             pgpu files on"
                .into(),
        );
    }
    let canon = path
        .canonicalize()
        .map_err(|_| format!("{} doesn't exist or can't be opened", path.display()))?;
    let meta = std::fs::metadata(&canon).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!(
            "{} is not a file; send one file at a time",
            path.display()
        ));
    }
    if own_file(agent, &canon) {
        return Err(format!(
            "{} is one of PocketGPU's own files (keys and settings), so it can't be sent",
            path.display()
        ));
    }
    let devices = crate::store::devices::list(&agent.db()).map_err(|e| format!("{e:#}"))?;
    let can_save = |permission: &str| {
        permission
            .parse::<Permission>()
            .is_ok_and(|p| p >= Permission::Control)
    };
    let targets: Vec<_> = match to {
        Some(query) => {
            let d = crate::daemon::devices::find(agent, query)?;
            if !can_save(&d.permission) {
                return Err(format!(
                    "{} can only view this machine; let it save files first with: \
                     pgpu devices set \"{}\" control",
                    d.name, d.name
                ));
            }
            vec![d]
        }
        None => devices
            .into_iter()
            .filter(|d| can_save(&d.permission) && !d.this_pc)
            .collect(),
    };
    let name = clean_name(&canon.file_name().unwrap_or_default().to_string_lossy());
    let stored = offers::Stored::new(
        &canon,
        &name,
        meta.len(),
        super::modified_ms(&meta),
        to.map(|_| targets[0].device_id.as_str()),
        now_ms(),
    )
    .and_then(|o| offers::insert(&agent.db(), &o).map(|()| o))
    .map_err(|e| format!("{e:#}"))?;
    let ids: Vec<String> = targets.iter().map(|d| d.device_id.clone()).collect();
    let names: Vec<String> = targets.iter().map(|d| d.name.clone()).collect();
    let who = if names.is_empty() {
        "nobody yet".to_string()
    } else {
        names.join(", ")
    };
    if let Err(e) = audit::record(&agent.db(), "local", Event::FileOffer, &shown(&canon), &who) {
        tracing::warn!("could not write audit entry: {e}");
    }
    let tickets = offers::tickets(&agent.db(), &ids).unwrap_or_else(|e| {
        tracing::warn!("no push for the offer: {e:#}");
        Vec::new()
    });
    let pushed = targets
        .iter()
        .filter(|d| tickets.iter().any(|(id, _)| *id == d.device_id))
        .map(|d| d.name.clone())
        .collect();
    offers::push(agent, tickets.into_iter().map(|(_, t)| t).collect());
    Ok(Response::Offered {
        name: stored.name,
        size: stored.size,
        to: names,
        expires_ms: stored.expires_ms,
        pushed,
    })
}

/// Inside PocketGPU's config, data or runtime folders, job logs aside.
fn own_file(agent: &Agent, path: &Path) -> bool {
    let jobs = agent.paths.jobs_dir();
    let jobs = jobs.canonicalize().unwrap_or(jobs);
    Context::of(agent)
        .agent_dirs
        .iter()
        .any(|d| guard::inside(path, d))
        && !guard::inside(path, &jobs)
}
