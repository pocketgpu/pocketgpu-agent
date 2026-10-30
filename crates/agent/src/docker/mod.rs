//! Docker containers for phones (F28). Docker access amounts to root on the machine, so it
//! stays off until `pgpu docker on`, and phones only list, start, stop and restart existing
//! containers and read their logs.

pub mod api;
pub mod handler;
pub mod logs;

use std::sync::Arc;

use pocketgpu_proto::local::Response;

use crate::daemon::state::Agent;

/// `pgpu docker [on | off | status]`: saved in `config.toml`; answers whether Docker can
/// be reached, so `on` says at once when it can't.
pub async fn local(agent: &Arc<Agent>, on: Option<bool>) -> Response {
    if let Some(on) = on {
        let saved = {
            let mut cfg = agent.config.write().expect("config lock poisoned");
            let mut next = cfg.clone();
            next.docker = on;
            next.save(&agent.paths.config_file()).map(|()| *cfg = next)
        };
        if let Err(e) = saved {
            return Response::Error(format!("{e:#}"));
        }
        let state = if on { "on" } else { "off" };
        let _ = crate::audit::record(
            &agent.db(),
            "local",
            crate::audit::Event::DockerAccess,
            "docker",
            state,
        );
    }
    let enabled = agent.config.read().expect("config lock poisoned").docker;
    let containers = api::list()
        .await
        .map(|l| l.len() as u32)
        .map_err(|e| e.to_string());
    Response::Docker {
        enabled,
        containers,
    }
}
