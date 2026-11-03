//! `pgpu power [status | on | off | cancel]`: restart, sleep and shut down from phones (F37).
//! Works only at the machine: phones can't turn it on.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::msg::control::{ComesBack, PowerState};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Action {
    /// Show whether phones may restart, sleep or shut down this machine (the default).
    Status,
    /// Let phones with Control do it, after Face ID and a 30-second countdown here.
    On,
    /// Stop phones from doing it, and cancel a countdown.
    Off,
    /// Cancel a restart, sleep or shutdown that is counting down.
    Cancel,
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let req = match action.unwrap_or(Action::Status) {
        Action::Status => Request::GetPower,
        Action::On => Request::SetPower { on: true },
        Action::Off => Request::SetPower { on: false },
        Action::Cancel => Request::CancelPower,
    };
    match client::call(paths, req).await? {
        Response::Power(s) => {
            if json {
                println!("{}", serde_json::to_string(&s)?);
            } else {
                print!("{}", render(&s));
            }
            Ok(())
        }
        Response::Ok => {
            println!("Cancelled. Nothing will happen.");
            Ok(())
        }
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
}

pub fn render(s: &PowerState) -> String {
    let mut out = if s.enabled {
        "Restart, sleep and shut down from phones are on: phones with Control can do it after \
         Face ID, with 30 seconds to cancel here.\nTurn it off with: pgpu power off\n"
            .to_string()
    } else {
        "Restart, sleep and shut down from phones are off.\nTurn them on with: pgpu power on\n"
            .to_string()
    };
    match s.comes_back {
        ComesBack::Yes => out.push_str("After a restart the agent starts again by itself.\n"),
        ComesBack::NoAutoSignIn => out.push_str(
            "After a restart this machine waits at the sign-in screen: turn on automatic \
             sign-in to come back by itself.\n",
        ),
        ComesBack::NoLinger => out.push_str(
            "After a restart the agent waits for you to log in. To start it at boot, run: \
             sudo loginctl enable-linger $USER\n",
        ),
        ComesBack::Unknown => {}
    }
    if let Some((action, _)) = s.pending {
        out.push_str(&format!(
            "A {} is counting down. Cancel it with: pgpu power cancel\n",
            action.as_str()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_to_change_it_and_whether_it_comes_back() {
        let s = PowerState {
            enabled: false,
            comes_back: ComesBack::NoLinger,
            allowed: vec![],
            pending: None,
            last: None,
        };
        let t = render(&s);
        assert!(
            t.contains("pgpu power on") && t.contains("enable-linger"),
            "{t}"
        );
    }
}
