//! `pgpu relay [show | set <url>… | clear]`: your own iroh-relay (F19).
//!
//! Relays only carry traffic when a direct connection fails, and they see encrypted bytes
//! only. New pairing codes carry the list, so phones paired afterwards use it too.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show the relays this machine uses (the default).
    Show,
    /// Use these relays instead of the default ones, for example https://relay.example.com
    Set {
        #[arg(required = true)]
        urls: Vec<String>,
    },
    /// Go back to the default relays.
    Clear,
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let (req, changed) = match action.unwrap_or(Action::Show) {
        Action::Show => (Request::GetRelays, false),
        Action::Set { urls } => (Request::SetRelays(urls), true),
        Action::Clear => (Request::SetRelays(Vec::new()), true),
    };
    let list = match client::call(paths, req).await? {
        Response::Relays(list) => list,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    let restarted = changed && restart();
    if json {
        println!(
            "{}",
            serde_json::json!({ "relays": list, "restarted": restarted })
        );
        return Ok(());
    }
    print!("{}", render(&list));
    if changed {
        if restarted {
            println!("Restarted the agent service to use them.");
        } else {
            println!("Restart the agent to use them: stop `pgpu agent` and start it again.");
        }
        println!(
            "Phones paired from now on get them from the pairing code; paired phones find them on their next connection."
        );
    }
    Ok(())
}

pub fn render(list: &[String]) -> String {
    if list.is_empty() {
        return "Using the default PocketGPU relays. They only carry traffic when a direct\n\
                connection fails, and see encrypted bytes only.\n\
                Use your own with: pgpu relay set https://relay.example.com\n"
            .into();
    }
    let mut out = String::from("Using your relays instead of the default ones:\n");
    for u in list {
        out += &format!("  {u}\n");
    }
    out
}

/// Restarts the user service when it runs, so the endpoint binds with the new relays.
fn restart() -> bool {
    crate::cli::service::state() == "active" && crate::cli::service::restart().is_ok()
}
