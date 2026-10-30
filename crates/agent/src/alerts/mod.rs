//! Alerts to paired phones through the push server (F08). Rule alerts (GPU temperature,
//! disk, Ollama) arrive with user story 5; job alerts are here.

pub mod channels;
pub mod client;
pub mod heartbeat;
pub mod rules;
pub mod sender;
pub mod settings;

use crate::daemon::state::Agent;

/// When held alerts resume (`PauseAlerts`), or `None` when they aren't held.
pub fn paused_until(agent: &Agent) -> Option<i64> {
    let until = agent
        .alerts_paused_until
        .load(std::sync::atomic::Ordering::Relaxed);
    (until > crate::store::db::now_ms()).then_some(until)
}
