//! Remote-use notices (F35, board 30): whoever is at this computer sees when another device
//! uses it. Every remote action already writes one audit entry, so `audit::record` hands each
//! entry here ([`observe`]); this module picks the ones worth a notice, groups repeats to one
//! a minute per device and kind, and shows them through the system's notifications
//! ([`show`]). Entries from the command line (`local`) and from this computer's own app
//! (`this_pc`) never show one.

pub mod show;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use pocketgpu_proto::local::NoticeLevel;
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::mpsc;

use crate::audit::Event;
use crate::daemon::state::Agent;

/// Repeats of one kind from one device within this long become one summary.
pub const GROUP_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Pair,
    Terminal,
    Kill,
    JobStop,
    Action,
    Container,
    Revoke,
    /// A restart, sleep or shutdown counting down (F37).
    Power,
}

impl Kind {
    /// Shown at every level, at once, never grouped.
    pub fn always(self) -> bool {
        matches!(self, Self::Pair | Self::Terminal | Self::Power)
    }
}

/// One remote action worth a notice.
#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    pub device: String,
    pub name: String,
    pub kind: Kind,
    pub target: String,
    pub result: String,
}

/// What the system shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    /// The pairing notice offers Revoke and It's mine for this device.
    pub revoke: Option<(String, String)>,
    /// The power countdown's notice offers Cancel (F37).
    pub cancel_power: bool,
}

/// What `audit::record` passes on.
#[derive(Debug)]
enum Msg {
    Notice(Remote),
    /// Any pairing, this computer's own app included: the other devices get a push.
    Paired {
        device: String,
        name: String,
        permission: String,
    },
}

static SINK: OnceLock<mpsc::UnboundedSender<Msg>> = OnceLock::new();

/// Starts the notices for this agent; entries recorded before this are not shown.
pub fn start(agent: Arc<Agent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    if SINK.set(tx).is_ok() {
        tokio::spawn(run(agent, rx));
    }
}

/// Called by `audit::record` for every entry. Cheap when the daemon isn't running.
pub fn observe(db: &Connection, device: &str, event: Event, target: &str, result: &str) {
    let Some(sink) = SINK.get() else { return };
    if event == Event::Pair && device != "local" {
        let _ = sink.send(Msg::Paired {
            device: device.into(),
            name: target.into(),
            permission: result.split(' ').next().unwrap_or("view").into(),
        });
    }
    if let Some(r) = remote(db, device, event, target, result) {
        let _ = sink.send(Msg::Notice(r));
    }
}

/// The entry as a remote action, or `None` when it shouldn't show a notice.
pub fn remote(
    db: &Connection,
    device: &str,
    event: Event,
    target: &str,
    result: &str,
) -> Option<Remote> {
    if device == "local" {
        return None;
    }
    let kind = kind_of(event, result)?;
    let (name, this_pc): (String, bool) = db
        .query_row(
            "SELECT name, this_pc FROM devices WHERE device_id = ?1",
            [device],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .ok()
        .flatten()
        // A device that just removed itself: its name is in the target.
        .unwrap_or_else(|| (target.to_string(), false));
    if this_pc {
        return None;
    }
    Some(Remote {
        device: device.into(),
        name,
        kind,
        target: target.into(),
        result: result.into(),
    })
}

/// The notice kind of an audit event, for successful ones only.
fn kind_of(event: Event, result: &str) -> Option<Kind> {
    Some(match event {
        Event::Pair => Kind::Pair,
        Event::ShellOpen if result == "ok" => Kind::Terminal,
        Event::Kill if result == "sigterm sent" => Kind::Kill,
        Event::JobStop if result == "ok" => Kind::JobStop,
        Event::ActionRun
            if result.starts_with("exit ")
                || result == "timed out"
                || result == "killed by a signal" =>
        {
            Kind::Action
        }
        Event::Container if result == "ok" => Kind::Container,
        Event::Revoke if result == REVOKED_BY_DEVICE => Kind::Revoke,
        Event::Power if result == crate::power::COUNTING_DOWN => Kind::Power,
        _ => return None,
    })
}

/// How to stop a power countdown from the notice: its button, or on macOS the command line.
#[cfg(not(target_os = "macos"))]
const CANCEL_HINT: &str = "To stop it, press Cancel.";
#[cfg(target_os = "macos")]
const CANCEL_HINT: &str = "To stop it, run: pgpu power cancel";

/// How to revoke from the notice: its button, or on macOS (no buttons yet) the app.
#[cfg(not(target_os = "macos"))]
const REVOKE_HINT: &str = "Revoke it.";
#[cfg(target_os = "macos")]
const REVOKE_HINT: &str = "Revoke it in PocketGPU, or with: pgpu devices revoke";

/// Changes the level, here at the computer only (no phone request exists for it).
pub fn set_level(agent: &Agent, level: NoticeLevel) -> anyhow::Result<NoticeLevel> {
    {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        let mut next = cfg.clone();
        next.notices = level;
        next.save(&agent.paths.config_file())?;
        *cfg = next;
    }
    let _ = crate::audit::record(
        &agent.db(),
        "local",
        Event::AlertSettings,
        &format!("notices {}", level.as_str()),
        "ok",
    );
    Ok(level)
}

/// The audit result of one device revoking another from a phone.
pub const REVOKED_BY_DEVICE: &str = "revoked from another device";

pub fn shown_at(level: NoticeLevel, kind: Kind) -> bool {
    kind.always() || level == NoticeLevel::All
}

/// The notice for one action.
pub fn text(r: &Remote, machine: &str) -> Notice {
    let who = &r.name;
    let (title, body) = match r.kind {
        Kind::Pair => {
            let perm = r.result.split(' ').next().unwrap_or("view");
            let what = match perm {
                "shell" => "Shell access, so it can open a terminal here",
                "control" => "Control access, so it can stop processes and run actions here",
                _ => "View access, so it can see this computer's stats",
            };
            (
                format!("New device paired: {who}"),
                format!("With {what}. Not you? {REVOKE_HINT}"),
            )
        }
        Kind::Terminal => (
            format!("{who} opened a terminal"),
            format!("On {machine}, as you. PocketGPU's tray shows who is connected."),
        ),
        Kind::Kill => (
            format!("{who} stopped {}", r.target),
            format!("On {machine}."),
        ),
        Kind::JobStop => (
            format!("{who} stopped {}", r.target),
            format!("On {machine}."),
        ),
        Kind::Action => (
            format!("{who} ran {}", action_name(&r.target)),
            format!("On {machine} · {}.", r.result),
        ),
        Kind::Container => (
            format!("{who} {}", container_text(&r.target)),
            format!("On {machine}."),
        ),
        Kind::Revoke => (
            format!("{who} revoked {}", r.target),
            format!("{} can no longer connect to {machine}.", r.target),
        ),
        Kind::Power => {
            let what = match r.target.as_str() {
                "restart" => "restarting this computer",
                "sleep" => "putting this computer to sleep",
                _ => "shutting down this computer",
            };
            (
                format!("{who} is {what} in 30 seconds"),
                format!("Save your work. {CANCEL_HINT}"),
            )
        }
    };
    Notice {
        title,
        body,
        revoke: (r.kind == Kind::Pair).then(|| (r.device.clone(), r.name.clone())),
        cancel_power: r.kind == Kind::Power,
    }
}

/// Several actions of one kind from one device in a minute.
pub fn summary(r: &Remote, count: u32, machine: &str) -> Notice {
    let who = &r.name;
    let first = match r.kind {
        Kind::Action => action_name(&r.target).to_string(),
        Kind::Container => r
            .target
            .split_once(' ')
            .map_or(r.target.clone(), |(_, id)| id.into()),
        _ => r.target.clone(),
    };
    let (verb, noun) = match r.kind {
        Kind::Kill => ("stopped", "processes"),
        Kind::JobStop => ("stopped", "jobs"),
        Kind::Action => ("ran", "quick actions"),
        Kind::Container => ("changed", "containers"),
        Kind::Revoke => ("revoked", "devices"),
        Kind::Pair | Kind::Terminal | Kind::Power => ("used", "things"),
    };
    Notice {
        title: format!("{who} {verb} {count} {noun}"),
        body: format!(
            "{first} and {} more on {machine}, in the last minute.",
            count - 1
        ),
        revoke: None,
        cancel_power: false,
    }
}

/// `#3 Restart vLLM` → `Restart vLLM`.
fn action_name(target: &str) -> &str {
    match target.split_once(' ') {
        Some((id, name)) if id.starts_with('#') => name,
        _ => target,
    }
}

/// `restart 3f2a…` → `restarted container 3f2a…`.
fn container_text(target: &str) -> String {
    let (action, id) = target.split_once(' ').unwrap_or(("changed", target));
    let short: String = id.chars().take(12).collect();
    let verb = match action {
        "start" => "started",
        "stop" => "stopped",
        "restart" => "restarted",
        other => other,
    };
    format!("{verb} container {short}")
}

/// Open grouping windows: per device and kind, when it opened, the first action and how
/// many so far.
pub type Windows = HashMap<(String, Kind), (Instant, Remote, u32)>;

/// What to do with one action, given the open windows: show it now, or count it.
pub fn group(windows: &mut Windows, r: Remote, now: Instant) -> Option<Remote> {
    if r.kind.always() {
        return Some(r);
    }
    let key = (r.device.clone(), r.kind);
    match windows.get_mut(&key) {
        Some((opened, _, count)) if now.duration_since(*opened) < GROUP_WINDOW => {
            *count += 1;
            None
        }
        _ => {
            windows.insert(key, (now, r.clone(), 1));
            Some(r)
        }
    }
}

/// Windows that are over: those with repeats give a summary of all of them.
pub fn flush(windows: &mut Windows, now: Instant) -> Vec<(Remote, u32)> {
    let mut out = Vec::new();
    windows.retain(|_, (opened, first, count)| {
        if now.duration_since(*opened) < GROUP_WINDOW {
            return true;
        }
        if *count > 1 {
            out.push((first.clone(), *count));
        }
        false
    });
    out
}

async fn run(agent: Arc<Agent>, mut rx: mpsc::UnboundedReceiver<Msg>) {
    let mut windows = Windows::new();
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            got = rx.recv() => {
                let r = match got {
                    None => return,
                    Some(Msg::Notice(r)) => r,
                    Some(Msg::Paired { device, name, permission }) => {
                        let agent = agent.clone();
                        tokio::spawn(async move {
                            match crate::alerts::new_device::send(&agent, &device, &name, &permission).await {
                                Ok(n) => tracing::info!(phones = n, "new-device push sent"),
                                Err(e) => tracing::warn!("new-device push not sent: {e:#}"),
                            }
                        });
                        continue;
                    }
                };
                let level = agent.config.read().expect("config lock poisoned").notices;
                if !shown_at(level, r.kind) {
                    continue;
                }
                if let Some(r) = group(&mut windows, r, Instant::now()) {
                    show::show(&agent, text(&r, &agent.name()));
                }
            }
            _ = tick.tick() => {
                for (first, count) in flush(&mut windows, Instant::now()) {
                    show::show(&agent, summary(&first, count, &agent.name()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
