//! `pgpu docker [on | off | status]`: Docker containers for paired phones (F28). Works only
//! at the machine: phones can't change it.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Action {
    /// Show whether phones may see containers (off by default) and whether Docker answers.
    Status,
    /// Let phones list containers and read their logs (View), and start, stop and restart them (Control).
    On,
    /// Hide containers from phones again.
    Off,
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let req = match action.unwrap_or(Action::Status) {
        Action::Status => Request::GetDocker,
        Action::On => Request::SetDocker { on: true },
        Action::Off => Request::SetDocker { on: false },
    };
    let (enabled, containers) = match client::call(paths, req).await? {
        Response::Docker {
            enabled,
            containers,
        } => (enabled, containers),
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        let (count, problem) = match &containers {
            Ok(n) => (Some(*n), None),
            Err(e) => (None, Some(e.as_str())),
        };
        println!(
            "{}",
            serde_json::json!({ "enabled": enabled, "containers": count, "problem": problem })
        );
    } else {
        print!("{}", render(enabled, &containers));
    }
    Ok(())
}

pub fn render(enabled: bool, containers: &Result<u32, String>) -> String {
    let docker = match containers {
        Ok(1) => "Docker answers with 1 container".to_string(),
        Ok(n) => format!("Docker answers with {n} containers"),
        Err(e) => format!("Docker can't be reached: {e}"),
    };
    if enabled {
        format!(
            "Docker for phones is on: paired phones see containers and their logs; Control phones can start, stop and restart them.\n\
             {docker}.\nTurn it off with: pgpu docker off\n"
        )
    } else {
        format!(
            "Docker for phones is off. {docker}.\n\
             Docker access is like root access, so phones only get it after: pgpu docker on\n"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_to_change_it_and_what_docker_said() {
        let off = render(false, &Ok(3));
        assert!(
            off.contains("pgpu docker on") && off.contains("3 containers"),
            "{off}"
        );
        let on = render(true, &Err("Docker isn't running on this machine".into()));
        assert!(
            on.contains("pgpu docker off") && on.contains("isn't running"),
            "{on}"
        );
    }
}
