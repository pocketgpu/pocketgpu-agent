//! `pgpu apps [status | on | off | add PORT [NAME] | remove PORT | hide PORT | show PORT]`:
//! the web apps on this machine that other devices can open (F42).

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::apps::Apps;
use pocketgpu_proto::local::{Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show the apps running here and whether other devices may open them (the default).
    Status,
    /// Let your phones and laptops with Full control open the apps running here.
    On,
    /// Stop other devices from opening apps here.
    Off,
    /// Look for an app on another port too: pgpu apps add 7000 "My demo"
    Add {
        port: u16,
        /// The name phones show; else the page's title.
        name: Option<String>,
    },
    /// Stop looking on a port added with `pgpu apps add`.
    Remove { port: u16 },
    /// Keep other devices out of one app.
    Hide { port: u16 },
    /// Let other devices open an app hidden with `pgpu apps hide` again.
    Show { port: u16 },
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let action = action.unwrap_or(Action::Status);
    let req = match &action {
        Action::Status => Request::GetApps,
        Action::On => Request::SetApps { on: true },
        Action::Off => Request::SetApps { on: false },
        Action::Add { port, name } => Request::AddApp {
            port: *port,
            name: name.clone(),
        },
        Action::Remove { port } => Request::RemoveApp { port: *port },
        Action::Hide { port } => Request::SetAppOff {
            port: *port,
            off: true,
        },
        Action::Show { port } => Request::SetAppOff {
            port: *port,
            off: false,
        },
    };
    let apps = match client::call(paths, req).await? {
        Response::Apps(a) => a,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::to_string(&apps)?);
    } else {
        print!("{}", render(&apps));
    }
    Ok(())
}

pub fn render(a: &Apps) -> String {
    let mut out = if a.enabled {
        "Apps: on. Your devices with Full control can open these.\n".to_string()
    } else {
        "Apps: off. Turn on with: pgpu apps on\n".to_string()
    };
    if a.apps.is_empty() {
        out.push_str(
            "No apps running now. Start Jupyter, TensorBoard or ComfyUI, or add a port: pgpu apps add 7000\n",
        );
        return out;
    }
    out.push('\n');
    for app in &a.apps {
        let note = match (app.off, app.added) {
            (true, _) => "  (hidden: pgpu apps show)",
            (false, true) => "  (added)",
            _ => "",
        };
        out.push_str(&format!("  {:<6} {}{note}\n", app.port, app.name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketgpu_proto::apps::{App, Kind};

    #[test]
    fn says_how_to_turn_on_and_lists_by_port() {
        let off = Apps {
            enabled: false,
            apps: vec![],
        };
        assert!(render(&off).contains("pgpu apps on"));
        let on = Apps {
            enabled: true,
            apps: vec![App {
                port: 8888,
                name: "Jupyter Lab".into(),
                kind: Kind::Jupyter,
                added: false,
                off: true,
            }],
        };
        let r = render(&on);
        assert!(
            r.contains("8888   Jupyter Lab  (hidden: pgpu apps show)"),
            "{r}"
        );
    }
}
