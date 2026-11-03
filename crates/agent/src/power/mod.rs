//! Power actions from phones (F37, board 31): restart, sleep or shut down after a 30-second
//! countdown that the person at the computer can cancel. Off until turned on here
//! (`pgpu power on` or the app's This PC); phones with Control may then ask. Every request,
//! cancel and outcome is audited, so the remote-use notices (F35) show it; the countdown's
//! notice has a Cancel button where the system allows one.

pub mod os;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pocketgpu_proto::msg::control::{PowerKind, PowerOutcome, PowerState};

use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

/// How long the person at the computer has to cancel.
pub const COUNTDOWN: Duration = Duration::from_secs(30);

/// What runs the system's command; faked in tests.
pub type Runner = fn(PowerKind) -> Result<(), String>;

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
}

/// The audit result of a countdown starting; the notices read it (F35).
pub const COUNTING_DOWN: &str = "in 30 s";

impl Power {
    pub fn pending(&self) -> Option<(PowerKind, i64)> {
        let inner = self.inner.lock().expect("power lock");
        inner.pending.as_ref().map(|p| (p.action, p.at_ms))
    }

    pub fn last(&self) -> Option<PowerOutcome> {
        self.inner.lock().expect("power lock").last.clone()
    }

    /// The countdown ended: runs the action unless it was cancelled meanwhile.
    fn fire(&self, agent: &Agent, id: u64, run: Runner) {
        let pending = {
            let mut inner = self.inner.lock().expect("power lock");
            match &inner.pending {
                Some(p) if p.id == id => inner.pending.take(),
                _ => None,
            }
        };
        let Some(p) = pending else { return };
        let result = match run(p.action) {
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
        self.inner.lock().expect("power lock").last = Some(PowerOutcome {
            action: p.action,
            by: p.by,
            at_ms: now_ms(),
            result,
        });
    }

    /// Cancels the countdown; `false` when nothing was pending.
    pub fn cancel(&self, agent: &Agent) -> bool {
        let p = self.inner.lock().expect("power lock").pending.take();
        let Some(p) = p else { return false };
        let _ = audit::record(
            &agent.db(),
            "local",
            Event::Power,
            p.action.as_str(),
            "cancelled",
        );
        self.inner.lock().expect("power lock").last = Some(PowerOutcome {
            action: p.action,
            by: p.by,
            at_ms: now_ms(),
            result: "cancelled".into(),
        });
        true
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
        agent.power.fire(&agent, id, run);
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
