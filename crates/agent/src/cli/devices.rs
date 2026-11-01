//! `pgpu devices [set <device> <permission>]` and `pgpu revoke <device>`.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::Permission;
use pocketgpu_proto::local::{DeviceInfo, Request, Response};

use crate::ipc::client;
use crate::store::db::now_ms;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Show paired phones (the default).
    List,
    /// Change what a phone may do: view, control or shell. Takes effect on its next request.
    Set {
        /// The phone's name, or the start of its key.
        device: String,
        #[arg(value_parser = parse_permission)]
        permission: Permission,
    },
}

fn parse_permission(s: &str) -> Result<Permission, String> {
    s.parse()
}

pub async fn run(paths: &Paths, json: bool, cmd: Option<Command>) -> Result<()> {
    let req = match cmd.unwrap_or(Command::List) {
        Command::List => Request::ListDevices,
        Command::Set { device, permission } => Request::SetDevicePermission { device, permission },
    };
    match client::call(paths, req).await? {
        Response::Devices(list) => {
            if json {
                let out: Vec<_> = list.iter().map(to_json).collect();
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                print!("{}", render(&list, now_ms()));
            }
        }
        Response::Device(d) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "ok": true, "device": to_json(&d) })
                );
            } else {
                println!("{} can now {}", d.name, describe(d.permission));
            }
        }
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
    Ok(())
}

pub async fn revoke(paths: &Paths, json: bool, device: String) -> Result<()> {
    match client::call(paths, Request::RevokeDevice { device }).await? {
        Response::Device(d) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "ok": true, "device": to_json(&d) })
                );
            } else {
                println!(
                    "Revoked {}. It was disconnected and must pair again.",
                    d.name
                );
            }
            Ok(())
        }
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
}

fn describe(p: Permission) -> &'static str {
    match p {
        Permission::View => "view stats, jobs and alerts",
        Permission::Control => "view and control (run actions, stop jobs, kill processes)",
        Permission::Shell => "view, control and use shell (create actions, terminal)",
    }
}

fn to_json(d: &DeviceInfo) -> serde_json::Value {
    serde_json::json!({
        "id": d.id, "name": d.name, "platform": d.platform,
        "permission": d.permission.as_str(), "paired_at_ms": d.paired_at_ms,
        "last_seen_ms": d.last_seen_ms, "connected": d.connected,
    })
}

fn ago(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

pub fn render(list: &[DeviceInfo], now_ms: i64) -> String {
    if list.is_empty() {
        return "No paired phones. Pair one with: pgpu pair\n".into();
    }
    let mut out = format!(
        "{:<20} {:<8} {:<10} {:<9} {:<12} {}\n",
        "NAME", "PLATFORM", "PERMISSION", "KEY", "LAST SEEN", ""
    );
    for d in list {
        let seen = if d.connected {
            "now".to_string()
        } else {
            d.last_seen_ms.map_or("never".into(), |t| ago(now_ms - t))
        };
        let name: String = d.name.chars().take(20).collect();
        out += &format!(
            "{:<20} {:<8} {:<10} {:<9} {:<12} {}\n",
            name,
            d.platform,
            d.permission.as_str(),
            &d.id[..8.min(d.id.len())],
            seen,
            if d.connected { "connected" } else { "" }
        );
    }
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n\nTo change what a phone can do: pgpu devices set <name> view|control|shell\n"
}
