//! `pgpu screen [status | on | off | stop [DEVICE]]`: whether your devices with Full control
//! may see this screen, and who looks now (F45).

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{Request, Response, ScreenStatus};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show whether devices may see this screen and who looks now (the default).
    Status,
    /// Let your phones and laptops with Full control see this screen. Looking only.
    On,
    /// Stop devices seeing this screen, and stop anyone looking now.
    Off,
    /// Stop a device looking now (its name), or everyone.
    Stop { device: Option<String> },
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let action = action.unwrap_or(Action::Status);
    let req = match &action {
        Action::Status => Request::GetScreen,
        Action::On => Request::SetScreen { on: true },
        Action::Off => Request::SetScreen { on: false },
        Action::Stop { device } => Request::StopScreen {
            device: device.clone(),
        },
    };
    let status = match client::call(paths, req).await? {
        Response::Screen(s) => s,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    print!("{}", words(&status, &action));
    Ok(())
}

fn words(s: &ScreenStatus, action: &Action) -> String {
    let mut out = String::new();
    match action {
        Action::Stop { .. } => out.push_str("Stopped.\n"),
        _ if s.enabled => out.push_str(
            "Seeing this screen is on: your devices with Full control can look (no clicks or keys).\n",
        ),
        _ => out.push_str("Seeing this screen is off. Turn it on with: pgpu screen on\n"),
    }
    if let Some(why) = &s.why {
        out.push_str(&format!("Note: {why}\n"));
    }
    if s.watching.is_empty() {
        if s.enabled {
            out.push_str("Nobody is looking now.\n");
        }
    } else {
        for w in &s.watching {
            out.push_str(&format!(
                "{} is looking now (Display {}). Stop it: pgpu screen stop \"{}\"\n",
                w.name,
                w.display + 1,
                w.name
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketgpu_proto::local::ScreenWatcher;

    #[test]
    fn says_who_looks() {
        let s = ScreenStatus {
            enabled: true,
            why: None,
            watching: vec![ScreenWatcher {
                device: "k".into(),
                name: "Sam's iPhone".into(),
                display: 0,
                since_ms: 0,
            }],
        };
        let w = words(&s, &Action::Status);
        assert!(w.contains("is on"));
        assert!(w.contains("Sam's iPhone is looking now (Display 1)"));
        let off = ScreenStatus {
            enabled: false,
            why: None,
            watching: vec![],
        };
        assert!(words(&off, &Action::Status).contains("pgpu screen on"));
    }
}
