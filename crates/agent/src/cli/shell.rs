//! `pgpu shell [on | off | status]`: terminal access for Shell phones (F16). Works only at
//! the machine: phones can't change it.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Action {
    /// Show whether terminal access is on (the default).
    Status,
    /// Let phones with Shell permission open a terminal, after Face ID or fingerprint on the phone.
    On,
    /// Block the terminal and close every open session at once.
    Off,
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let req = match action.unwrap_or(Action::Status) {
        Action::Status => Request::GetShell,
        Action::On => Request::SetShell { on: true },
        Action::Off => Request::SetShell { on: false },
    };
    let (enabled, sessions) = match client::call(paths, req).await? {
        Response::Shell { enabled, sessions } => (enabled, sessions),
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!(
            "{}",
            serde_json::json!({ "enabled": enabled, "sessions": sessions })
        );
    } else {
        print!("{}", render(enabled, sessions));
    }
    Ok(())
}

pub fn render(enabled: bool, sessions: u32) -> String {
    if !enabled {
        return "Terminal access is off. Phones can't open a terminal on this machine.\n\
                Turn it on with: pgpu shell on\n"
            .into();
    }
    let open = match sessions {
        0 => "no sessions open".to_string(),
        1 => "1 session open".to_string(),
        n => format!("{n} sessions open"),
    };
    format!(
        "Terminal access is on: phones with Shell permission can open a terminal after Face ID or fingerprint ({open}).\n\
         Turn it off with: pgpu shell off\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_to_change_it() {
        assert!(render(false, 0).contains("pgpu shell on"));
        let on = render(true, 2);
        assert!(
            on.contains("2 sessions open") && on.contains("pgpu shell off"),
            "{on}"
        );
        assert!(render(true, 1).contains("1 session open"));
    }
}
