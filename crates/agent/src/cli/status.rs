//! `pgpu` with no arguments: agent, connection and GPU summary.

use anyhow::{Result, bail};
use pocketgpu_proto::local::{Request, Response, Status};

use crate::ipc::client;
use crate::store::paths::Paths;

pub async fn run(paths: &Paths, json: bool) -> Result<()> {
    let status = match client::call(paths, Request::Status).await? {
        Response::Status(s) => s,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&to_json(&status))?);
    } else {
        print!("{}", render(&status));
    }
    Ok(())
}

pub fn render(s: &Status) -> String {
    let mut out = String::new();
    let plural = |n: u32| if n == 1 { "device" } else { "devices" };
    let relayed = s.connected_devices.saturating_sub(s.direct_devices);
    let devices = match (s.paired_devices, s.direct_devices, relayed) {
        (0, _, _) => "no phones paired yet · run `pgpu pair`".to_string(),
        (p, 0, 0) => format!("{p} paired, none connected"),
        (_, d, 0) => format!("direct to {d} {}", plural(d)),
        (_, 0, r) => format!("relayed to {r} {}", plural(r)),
        (_, d, r) => format!("direct to {d}, relayed to {r} {}", plural(r)),
    };
    out += &format!("{:<11} running · {devices}\n", "Agent");
    out += &format!("{:<11} {} ({})\n", "Machine", s.name, s.fingerprint);
    if s.gpus.is_empty() {
        let why = s.gpu_problem.as_deref().unwrap_or("no NVIDIA GPU found");
        out += &format!("{:<11} none · {why}\n", "GPU");
    }
    for g in &s.gpus {
        let live = s
            .latest
            .as_ref()
            .and_then(|l| l.gpus.iter().find(|x| x.index == g.index));
        let mut parts = vec![g.name.replace("NVIDIA GeForce ", "")];
        if let Some(l) = live {
            if let Some(u) = l.util_pct {
                parts.push(format!("{u}%"));
            }
            if let Some(v) = l.vram_used_mb {
                parts.push(format!(
                    "{:.1}/{:.0} GB",
                    v as f64 / 1024.0,
                    g.vram_total_mb as f64 / 1024.0
                ));
            }
            if let Some(t) = l.temp_c {
                parts.push(format!("{t}°C"));
            }
            if let Some(p) = l.power_w {
                parts.push(format!("{p} W"));
            }
        }
        out += &format!("{:<11} {}\n", format!("GPU {}", g.index), parts.join(" · "));
    }
    let ollama = match s.latest.as_ref().and_then(|l| l.ollama_up) {
        Some(true) => "running",
        Some(false) => "not running",
        None => "not checked yet",
    };
    out += &format!("{:<11} {ollama}\n", "Ollama");
    out
}

pub fn to_json(s: &Status) -> serde_json::Value {
    serde_json::json!({
        "agent": { "running": true, "version": s.version, "id": s.agent_id, "name": s.name },
        "connection": {
            "paired": s.paired_devices,
            "connected": s.connected_devices,
            "direct": s.direct_devices,
        },
        "gpus": s.gpus.iter().map(|g| {
            let live = s.latest.as_ref().and_then(|l| l.gpus.iter().find(|x| x.index == g.index));
            serde_json::json!({
                "index": g.index, "name": g.name, "vram_total_mb": g.vram_total_mb,
                "util_pct": live.and_then(|l| l.util_pct), "vram_used_mb": live.and_then(|l| l.vram_used_mb),
                "temp_c": live.and_then(|l| l.temp_c), "power_w": live.and_then(|l| l.power_w),
            })
        }).collect::<Vec<_>>(),
        "gpu_problem": s.gpu_problem,
        // Jobs arrive with the job watcher (F07).
        "jobs_running": 0,
        "ollama": { "running": s.latest.as_ref().and_then(|l| l.ollama_up) },
    })
}
