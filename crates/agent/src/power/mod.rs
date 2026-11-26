//! Power actions from phones (F37, board 31): restart, sleep or shut down after a 30-second
//! countdown that the person at the computer can cancel. Off until turned on here
//! (`pgpu power on` or the app's This PC); phones with Control may then ask. Every request,
//! cancel and outcome is audited, so the remote-use notices (F35) show it; the countdown's
//! notice has a Cancel button where the system allows one.

pub mod os;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pocketgpu_proto::Permission;
use pocketgpu_proto::msg::control::{PowerKind, PowerOutcome, PowerState};

use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

/// How long the person at the computer has to cancel.
pub const COUNTDOWN: Duration = Duration::from_secs(30);

/// What runs the system's command; faked in tests.
pub type Runner = fn(PowerKind) -> Result<(), String>;

/// The system's commands, or with `PGPU_FAKE_POWER` set (end-to-end tests on CI runners,
/// which must not restart) only a log line.
pub fn runner() -> Runner {
    if std::env::var_os("PGPU_FAKE_POWER").is_some() {
        fake
    } else {
        os::run
    }
}

fn fake(action: PowerKind) -> Result<(), String> {
    tracing::info!("PGPU_FAKE_POWER: would {} now", action.as_str());
    Ok(())
}

#[derive(Default)]
pub struct Power {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    pending: Option<Pending>,
    last: Option<PowerOutcome>,
    next_id: u64,
}

struct Pending {
    id: u64,
    action: PowerKind,
    at_ms: i64,
    by: String,
    /// The key of the device that asked: the countdown ends with its access.
    device: String,
}

/// The audit result of a countdown starting; the notices read it (F35).
pub const COUNTING_DOWN: &str = "in 30 s";

/// The audit result of a countdown that ended because the device that asked was removed,
/// its guest time ended or it lost Control.
pub const ASKER_GONE: &str = "cancelled: the device that asked no longer has Control";

/// How long after a restart or shut down started the agent looks again: still running then,
/// the machine stayed on. A Mac asks apps to quit first, and one with unsaved work can stop
/// it after the system's command already succeeded.
#[cfg(not(test))]
const STILL_UP: Duration = Duration::from_secs(120);
#[cfg(test)]
const STILL_UP: Duration = Duration::from_secs(1);

/// The outcome of a restart or shut down that started but left the machine on.
pub const STILL_ON: &str = "still on 2 minutes later; an app may be asking to save its work";

/// Whether the device that asked may still: paired, in its guest time, with Control.
fn may_still(agent: &Agent, device: &str) -> bool {
    device
        .parse::<iroh::PublicKey>()
        .ok()
        .and_then(|k| agent.permission_of(&k))
        .is_some_and(|p| p >= Permission::Control)
}

impl Power {
    pub fn pending(&self) -> Option<(PowerKind, i64)> {
        let inner = self.inner.lock().expect("power lock");
        inner.pending.as_ref().map(|p| (p.action, p.at_ms))
    }

    pub fn last(&self) -> Option<PowerOutcome> {
        self.inner.lock().expect("power lock").last.clone()
    }

    /// The countdown ended: runs the action unless it was cancelled meanwhile, or the device
    /// that asked may no longer (revoked, its guest time over, or less than Control now).
    /// A restart or shut down that started gives its outcome's time, for `still_up`.
    fn fire(&self, agent: &Agent, id: u64, run: Runner) -> Option<i64> {
        let pending = {
            let mut inner = self.inner.lock().expect("power lock");
            match &inner.pending {
                Some(p) if p.id == id => inner.pending.take(),
                _ => None,
            }
        };
        let p = pending?;
        if !may_still(agent, &p.device) {
            self.cancelled(agent, p, ASKER_GONE);
            return None;
        }
        let ran = run(p.action);
        let goes_down = ran.is_ok() && matches!(p.action, PowerKind::Restart | PowerKind::ShutDown);
        let result = match ran {
            Ok(()) => "started".to_string(),
            Err(e) => e,
        };
        let _ = audit::record(
            &agent.db(),
            "local",
            Event::Power,
            p.action.as_str(),
            &result,
        );
        let at_ms = now_ms();
        self.inner.lock().expect("power lock").last = Some(PowerOutcome {
            action: p.action,
            by: p.by,
            at_ms,
            result,
        });
        goes_down.then_some(at_ms)
    }

    /// Still running `STILL_UP` after the restart or shut down that started at `at_ms`: it
    /// didn't happen, so phones see that instead of "started".
    fn still_up(&self, agent: &Agent, at_ms: i64) {
        let action = {
            let mut inner = self.inner.lock().expect("power lock");
            match inner.last.as_mut() {
                Some(l) if l.at_ms == at_ms && l.result == "started" => {
                    l.result = STILL_ON.into();
                    l.action
                }
                _ => return,
            }
        };
        let _ = audit::record(
            &agent.db(),
            "local",
            Event::Power,
            action.as_str(),
            STILL_ON,
        );
    }

    /// Cancels the countdown; `false` when nothing was pending.
    pub fn cancel(&self, agent: &Agent) -> bool {
        let p = self.inner.lock().expect("power lock").pending.take();
        let Some(p) = p else { return false };
        self.cancelled(agent, p, "cancelled");
        true
    }

    /// Cancels the countdown if `device` (a key) asked for it; called when that device is
    /// removed, its guest time ends or it is set below Control. `false` when it had nothing
    /// pending.
    pub fn cancel_for(&self, agent: &Agent, device: &str) -> bool {
        let p = {
            let mut inner = self.inner.lock().expect("power lock");
            match &inner.pending {
                Some(p) if p.device == device => inner.pending.take(),
                _ => None,
            }
        };
        let Some(p) = p else { return false };
        self.cancelled(agent, p, ASKER_GONE);
        true
    }

    /// A countdown that won't run: audited with `why`; phones see it as cancelled.
    fn cancelled(&self, agent: &Agent, p: Pending, why: &str) {
        let _ = audit::record(&agent.db(), "local", Event::Power, p.action.as_str(), why);
        self.inner.lock().expect("power lock").last = Some(PowerOutcome {
            action: p.action,
            by: p.by,
            at_ms: now_ms(),
            result: "cancelled".into(),
        });
    }
}

/// Starts the countdown for `action`, asked by `device` (its key) called `by`.
pub fn schedule(
    agent: &Arc<Agent>,
    device: &str,
    by: &str,
    action: PowerKind,
    wait: Duration,
    run: Runner,
) -> Result<i64, String> {
    let (id, at_ms) = {
        let mut inner = agent.power.inner.lock().expect("power lock");
        if let Some(p) = &inner.pending {
            return Err(format!(
                "a {} is already counting down on this machine",
                p.action.as_str()
            ));
        }
        inner.next_id += 1;
        let id = inner.next_id;
        let at_ms = now_ms() + wait.as_millis() as i64;
        inner.pending = Some(Pending {
            id,
            action,
            at_ms,
            by: by.into(),
            device: device.into(),
        });
        (id, at_ms)
    };
    let _ = audit::record(
        &agent.db(),
        device,
        Event::Power,
        action.as_str(),
        COUNTING_DOWN,
    );
    let agent = agent.clone();
    tokio::spawn(async move {
        tokio::time::sleep(wait).await;
        if let Some(at_ms) = agent.power.fire(&agent, id, run) {
            tokio::time::sleep(STILL_UP).await;
            agent.power.still_up(&agent, at_ms);
        }
    });
    Ok(at_ms)
}

pub fn state(agent: &Agent) -> PowerState {
    PowerState {
        enabled: agent.config.read().expect("config lock poisoned").power,
        comes_back: os::comes_back(),
        allowed: os::allowed(),
        pending: agent.power.pending(),
        last: agent.power.last(),
    }
}

/// Turns power actions from phones on or off, here at the computer only.
pub fn set_enabled(agent: &Agent, on: bool) -> anyhow::Result<()> {
    {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        let mut next = cfg.clone();
        next.power = on;
        next.save(&agent.paths.config_file())?;
        *cfg = next;
    }
    if !on {
        agent.power.cancel(agent);
    }
    let _ = audit::record(
        &agent.db(),
        "local",
        Event::PowerAccess,
        if on { "on" } else { "off" },
        "ok",
    );
    Ok(())
}

/// A phone's request: checked against the setting and what the system allows.
pub fn request(
    agent: &Arc<Agent>,
    device: &str,
    by: &str,
    action: PowerKind,
    wait: Duration,
    run: Runner,
) -> Result<i64, String> {
    if !agent.config.read().expect("config lock poisoned").power {
        return Err(
            "restart and shut down from phones are off on this machine. \
                    Turn them on there with: pgpu power on"
                .into(),
        );
    }
    if !os::allowed().contains(&action) {
        return Err(format!(
            "this machine doesn't let the agent's user {} it",
            action.as_str()
        ));
    }
    schedule(agent, device, by, action, wait, run)
}

#[cfg(test)]
mod tests;
