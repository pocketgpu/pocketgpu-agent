//! Remote-use notices (F35, board 30): whoever is at this computer sees when another device
//! uses it. Every remote action already writes one audit entry, so `audit::record` hands each
//! entry here ([`observe`]); this module picks the ones worth a notice, groups repeats to one
//! a minute per device and kind (files: one notice for a batch sent within seconds of each
//! other), and shows them through the system's notifications ([`show`]). Entries from the command line (`local`) and from this computer's own app
//! (`this_pc`) never show one.

pub mod show;
mod text;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use pocketgpu_proto::local::NoticeLevel;
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::mpsc;

use crate::audit::Event;
use crate::daemon::state::Agent;

pub use text::{summary, text};

/// Repeats of one kind from one device within this long become one summary.
pub const GROUP_WINDOW: Duration = Duration::from_secs(60);

/// File notices wait this long after the last file from a device (and at most
/// [`GROUP_WINDOW`] after the first), so a batch is one notice: "Pixel 8 sent 3 files".
pub const HOLD: Duration = Duration::from_secs(10);

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
    /// A device sent a file here (F40).
    FileSent,
    /// A device saved a file from here.
    FileTaken,
    /// A device read the clipboard (F41).
    ClipRead,
    /// A device opened one of the machine's web apps (F42).
    AppOpen,
    /// A guest's time was up and its access ended (F44).
    GuestEnded,
}

impl Kind {
    /// Shown at every level, the fewer one too.
    pub fn always(self) -> bool {
        matches!(
            self,
            Self::Pair
                | Self::Terminal
                | Self::Power
                | Self::FileSent
                | Self::ClipRead
                | Self::AppOpen
                | Self::GuestEnded
        )
    }

    /// Shown at once each time, never grouped with others.
    pub fn alone(self) -> bool {
        matches!(self, Self::Pair | Self::Terminal | Self::Power)
    }

    /// Held back for [`HOLD`] and shown once for the whole batch, not first one by one.
    pub fn held(self) -> bool {
        matches!(self, Self::FileSent | Self::FileTaken)
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
        Event::Revoke if result == crate::pair::guest::GUEST_ENDED => Kind::GuestEnded,
        Event::Power if result == crate::power::COUNTING_DOWN => Kind::Power,
        Event::FilePut if result == "ok" => Kind::FileSent,
        Event::FileGet if result == crate::audit::STARTED || result == "ok" => Kind::FileTaken,
        Event::ClipGet if result == "ok" => Kind::ClipRead,
        Event::AppOpen if result == "ok" => Kind::AppOpen,
        _ => return None,
    })
}

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

/// One open grouping window.
#[derive(Debug, Clone)]
pub struct Open {
    opened: Instant,
    last: Instant,
    first: Remote,
    count: u32,
}

impl Open {
    fn over(&self, now: Instant) -> bool {
        now.duration_since(self.opened) >= GROUP_WINDOW
            || (self.first.kind.held() && now.duration_since(self.last) >= HOLD)
    }
}

/// Open grouping windows, per device and kind.
pub type Windows = HashMap<(String, Kind), Open>;

/// What to do with one action, given the open windows: show it now, or count it. Call
/// [`flush`] first, so a window that is over gives its notice before a new one opens.
pub fn group(windows: &mut Windows, r: Remote, now: Instant) -> Option<Remote> {
    if r.kind.alone() {
        return Some(r);
    }
    let key = (r.device.clone(), r.kind);
    match windows.get_mut(&key) {
        Some(open) if !open.over(now) => {
            open.count += 1;
            open.last = now;
            None
        }
        _ => {
            let held = r.kind.held();
            let open = Open {
                opened: now,
                last: now,
                first: r.clone(),
                count: 1,
            };
            windows.insert(key, open);
            (!held).then_some(r)
        }
    }
}

/// Windows that are over: those with repeats give a summary of all of them (count over
/// 1), a held one its only action (count 1).
pub fn flush(windows: &mut Windows, now: Instant) -> Vec<(Remote, u32)> {
    let mut out = Vec::new();
    windows.retain(|_, open| {
        if !open.over(now) {
            return true;
        }
        if open.count > 1 || open.first.kind.held() {
            out.push((open.first.clone(), open.count));
        }
        false
    });
    out
}

/// The notice for what [`flush`] gave.
fn flushed(first: &Remote, count: u32, machine: &str) -> Notice {
    if count > 1 {
        summary(first, count, machine)
    } else {
        text(first, machine)
    }
}

async fn run(agent: Arc<Agent>, mut rx: mpsc::UnboundedReceiver<Msg>) {
    let mut windows = Windows::new();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
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
                let now = Instant::now();
                for (first, count) in flush(&mut windows, now) {
                    show::show(&agent, flushed(&first, count, &agent.name()));
                }
                if let Some(r) = group(&mut windows, r, now) {
                    show::show(&agent, text(&r, &agent.name()));
                }
            }
            _ = tick.tick() => {
                for (first, count) in flush(&mut windows, Instant::now()) {
                    show::show(&agent, flushed(&first, count, &agent.name()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
