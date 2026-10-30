//! `pgpu alerts [list | set <kind> <value> | on <kind> | off <kind>]`.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::msg::control::{AlertKind, AlertRule};

use crate::alerts::settings::limits;
use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show every alert rule (the default).
    List,
    /// Set a limit and turn the rule on: gpu-temp °C, disk-full %, offline minutes.
    Set { kind: String, value: f32 },
    /// Turn a rule on.
    On { kind: String },
    /// Turn a rule off.
    Off { kind: String },
}

pub fn parse_kind(s: &str) -> Result<AlertKind> {
    Ok(match s {
        "job-end" | "job" => AlertKind::JobEnd,
        "gpu-temp" | "temp" => AlertKind::GpuTemp,
        "disk-full" | "disk" => AlertKind::DiskFull,
        "offline" => AlertKind::Offline,
        "ollama-down" | "ollama" => AlertKind::OllamaDown,
        "generic-text" | "generic" => AlertKind::GenericText,
        _ => bail!(
            "unknown alert {s:?}; use job-end, gpu-temp, disk-full, offline, ollama-down or generic-text"
        ),
    })
}

fn name(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::JobEnd => "job-end",
        AlertKind::GpuTemp => "gpu-temp",
        AlertKind::DiskFull => "disk-full",
        AlertKind::Offline => "offline",
        AlertKind::OllamaDown => "ollama-down",
        AlertKind::Unknown => "unknown",
        AlertKind::GenericText => "generic-text",
    }
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let req = match action.unwrap_or(Action::List) {
        Action::List => Request::GetAlerts,
        Action::Set { kind, value } => {
            let kind = parse_kind(&kind)?;
            if limits(kind).is_none() {
                bail!(
                    "{} has no limit to set; use `pgpu alerts on {}`",
                    name(kind),
                    name(kind)
                );
            }
            Request::SetAlerts(vec![AlertRule {
                kind,
                enabled: true,
                threshold: Some(value),
            }])
        }
        Action::On { kind } => Request::SetAlerts(vec![AlertRule {
            kind: parse_kind(&kind)?,
            enabled: true,
            threshold: None,
        }]),
        Action::Off { kind } => Request::SetAlerts(vec![AlertRule {
            kind: parse_kind(&kind)?,
            enabled: false,
            threshold: None,
        }]),
    };
    let rules = match client::call(paths, req).await? {
        Response::Alerts(r) => r,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        let out: Vec<_> = rules
            .iter()
            .map(|r| serde_json::json!({ "kind": name(r.kind), "enabled": r.enabled, "threshold": r.threshold }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", render(&rules));
    }
    Ok(())
}

pub fn render(rules: &[AlertRule]) -> String {
    let mut out = String::new();
    for r in rules {
        let state = if r.enabled { "on" } else { "off" };
        let limit = match (limits(r.kind), r.threshold) {
            (Some((_, _, unit)), Some(v)) => format!("{v} {unit}"),
            _ => String::new(),
        };
        // Job alerts are free, and generic text is a setting, not an alert.
        let plan = match r.kind {
            AlertKind::JobEnd | AlertKind::GenericText => "",
            _ => "Pro",
        };
        out += &format!("{:<12} {:<4} {:<13} {plan}\n", name(r.kind), state, limit);
    }
    out.trim_end().to_string() + "\n"
}
