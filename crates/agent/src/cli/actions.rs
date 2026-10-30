//! `pgpu actions [list | add | edit | rm]`: quick actions the phone can run with one tap.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{ActionEdit, Request, Response};
use pocketgpu_proto::msg::control::{Action, NewAction};

use crate::actions::ICONS;
use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Show every action (the default).
    List,
    /// Save a command: pgpu actions add "Restart vLLM" [--risky] [--icon I] -- <command>
    Add {
        name: String,
        /// Ask for Face ID or fingerprint on the phone before running it.
        #[arg(long)]
        risky: bool,
        /// One of the icons the phone shows; `pgpu actions icons` lists them.
        #[arg(long)]
        icon: Option<String>,
        /// Stop it after this many seconds (default 600).
        #[arg(long)]
        timeout: Option<u32>,
        /// The command, after `--`; it runs through your shell.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Change an action, by id or name; a command after `--` replaces the old one.
    Edit {
        action: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, conflicts_with = "not_risky")]
        risky: bool,
        #[arg(long)]
        not_risky: bool,
        #[arg(long)]
        icon: Option<String>,
        #[arg(long)]
        timeout: Option<u32>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Delete an action, by id or name.
    Rm { action: String },
    /// List the icons an action can use.
    Icons,
}

pub async fn run(paths: &Paths, json: bool, cmd: Option<Command>) -> Result<()> {
    let req = match cmd.unwrap_or(Command::List) {
        Command::Icons => {
            println!("{}", ICONS.join("\n"));
            return Ok(());
        }
        Command::List => Request::ListActions,
        Command::Add {
            name,
            risky,
            icon,
            timeout,
            command,
        } => Request::AddAction(NewAction {
            name,
            command: command.join(" "),
            icon: icon.unwrap_or_default(),
            risky,
            timeout_s: timeout,
        }),
        Command::Edit {
            action,
            name,
            risky,
            not_risky,
            icon,
            timeout,
            command,
        } => Request::EditAction(ActionEdit {
            action,
            name,
            command: (!command.is_empty()).then(|| command.join(" ")),
            icon,
            risky: match (risky, not_risky) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            },
            timeout_s: timeout,
        }),
        Command::Rm { action } => Request::RemoveAction { action },
    };
    match client::call(paths, req).await? {
        Response::Actions(list) => print_list(&list, json),
        Response::Action(a) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&to_json(&a))?);
            } else {
                println!("Saved #{} {}", a.id, a.name);
            }
            Ok(())
        }
        Response::Ok => {
            if json {
                println!("{}", serde_json::json!({ "ok": true }));
            } else {
                println!("Deleted");
            }
            Ok(())
        }
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
}

fn to_json(a: &Action) -> serde_json::Value {
    serde_json::json!({
        "id": a.id,
        "name": a.name,
        "command": a.command,
        "icon": a.icon,
        "risky": a.risky,
        "timeout_s": a.timeout_s,
    })
}

fn print_list(list: &[Action], json: bool) -> Result<()> {
    if json {
        let out: Vec<_> = list.iter().map(to_json).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", render(list));
    }
    Ok(())
}

pub fn render(list: &[Action]) -> String {
    if list.is_empty() {
        return "No actions yet. Add one with: pgpu actions add \"Name\" -- <command>\n".into();
    }
    let mut out = format!(
        "{:>3}  {:<24} {:<5} {:>7}  {}\n",
        "ID", "NAME", "RISKY", "TIMEOUT", "COMMAND"
    );
    for a in list {
        let risky = if a.risky { "yes" } else { "" };
        let command: String = a.command.chars().take(60).collect();
        out += &format!(
            "{:>3}  {:<24} {:<5} {:>6}s  {command}\n",
            a.id, a.name, risky, a.timeout_s
        );
    }
    out
}
