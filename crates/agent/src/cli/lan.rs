//! `pgpu lan [status | on | off]`: whether this computer can be found on the same Wi-Fi (F47).

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{LanStatus, Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show whether phones on this Wi-Fi can find this computer (the default).
    Status,
    /// Let phones on this Wi-Fi find this computer while `pgpu pair` runs, and let paired
    /// phones reconnect here without internet.
    On,
    /// Show nothing on the Wi-Fi. Pairing with the code still works.
    Off,
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let req = match action.unwrap_or(Action::Status) {
        Action::Status => Request::GetLan,
        Action::On => Request::SetLan { on: true },
        Action::Off => Request::SetLan { on: false },
    };
    let status = match client::call(paths, req).await? {
        Response::Lan(s) => s,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::to_string(&status)?);
    } else {
        print!("{}", words(&status));
    }
    Ok(())
}

fn words(s: &LanStatus) -> String {
    match (s.on, s.waiting) {
        (false, _) => {
            "Find on Wi-Fi is off. The code still works. Turn it on: pgpu lan on\n".into()
        }
        (true, true) => "Find on Wi-Fi is on, and `pgpu pair` is waiting: phones on this Wi-Fi \
                         can ask to connect.\n"
            .into(),
        (true, false) => "Find on Wi-Fi is on. Phones see this computer's name only while \
                          `pgpu pair` runs.\n"
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_says_what_it_means() {
        let off = words(&LanStatus {
            on: false,
            waiting: false,
        });
        assert!(off.contains("pgpu lan on") && off.contains("code still works"));
        let quiet = words(&LanStatus {
            on: true,
            waiting: false,
        });
        assert!(quiet.contains("only while `pgpu pair` runs"));
        let waiting = words(&LanStatus {
            on: true,
            waiting: true,
        });
        assert!(waiting.contains("can ask to connect"));
    }
}
