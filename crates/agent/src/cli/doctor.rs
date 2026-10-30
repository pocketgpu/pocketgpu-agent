//! `pgpu doctor`: checks what PocketGPU needs and says how to fix what's missing.
//! Exits with 1 when any check fails. Runs as root too, to explain why that won't work.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use iroh::endpoint::presets;
use iroh::{Endpoint, RelayMode, RelayUrl, Watcher as _};
use pocketgpu_proto::local::{Request, Response};
use serde::Serialize;

use super::Silent;
use crate::collect::{GpuCollector, nvml::NvmlGpus, ollama};
use crate::daemon::root::{self, RootCheck};
use crate::ipc::client;
use crate::store::config::Config;
use crate::store::paths::Paths;

/// Less free space than this on the data disk fails the check.
const MIN_FREE_GB: f64 = 1.0;
const NET_REPORT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub check: &'static str,
    pub ok: bool,
    pub detail: String,
}

impl Check {
    fn ok(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            ok: true,
            detail: detail.into(),
        }
    }

    fn fail(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            ok: false,
            detail: detail.into(),
        }
    }
}

pub async fn run(paths: &Paths, json: bool) -> Result<()> {
    let config = Config::load(&paths.config_file()).unwrap_or_default();
    let (connection, ollama) = tokio::join!(connection(&config), ollama(&config));
    let mut checks = vec![user(), agent(paths).await, gpu()];
    checks.extend(connection);
    checks.push(ollama);
    checks.push(disk(&paths.data_dir));
    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        print!("{}", render(&checks));
    }
    let failed = checks.iter().filter(|c| !c.ok).count();
    if failed > 0 {
        return Err(Silent(1).into());
    }
    Ok(())
}

pub fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        let mark = if c.ok { "✓" } else { "✗" };
        out += &format!("{mark} {:<11} {}\n", c.check, c.detail);
    }
    let failed = checks.iter().filter(|c| !c.ok).count();
    if failed > 0 {
        let s = if failed == 1 { "" } else { "s" };
        out += &format!("\n{failed} check{s} failed.\n");
    }
    out
}

fn user() -> Check {
    match root::check() {
        #[cfg(windows)]
        Ok(RootCheck::NotRoot) if crate::winsys::unelevated::is_child() => Check::ok(
            "User",
            "started as Administrator; runs without admin rights",
        ),
        Ok(RootCheck::NotRoot) if cfg!(windows) => Check::ok("User", "not Administrator"),
        Ok(RootCheck::NotRoot) => Check::ok("User", "not root"),
        Ok(RootCheck::RootInContainer) => {
            Check::ok("User", "root inside a container; the terminal stays off")
        }
        Err(_) if cfg!(windows) => Check::fail(
            "User",
            "running as Administrator. Use a normal terminal, so a phone can never act as Administrator",
        ),
        Err(_) => Check::fail(
            "User",
            "running as root. Run PocketGPU as your normal user, so a phone can never act as root",
        ),
    }
}

async fn agent(paths: &Paths) -> Check {
    match client::call(paths, Request::Status).await {
        Ok(Response::Status(s)) => {
            let phones = match s.paired_devices {
                0 => "no phones paired yet · run `pgpu pair`".to_string(),
                1 => "1 phone paired".to_string(),
                n => format!("{n} phones paired"),
            };
            Check::ok("Agent", format!("running {} · {phones}", s.version))
        }
        Ok(other) => Check::fail("Agent", format!("unexpected reply: {other:?}")),
        Err(_) => Check::fail(
            "Agent",
            "not running. Start it with `pgpu service install` (or `pgpu agent` in a container)",
        ),
    }
}

fn gpu() -> Check {
    if std::env::var("PGPU_FAKE_GPU").is_ok_and(|v| v != "0") {
        return Check::ok("GPU", "fake GPUs (PGPU_FAKE_GPU is set)");
    }
    let gpus = NvmlGpus::init();
    if let Some(problem) = gpus.problem() {
        return Check::fail("GPU", problem);
    }
    let info = gpus.info();
    let Some(first) = info.first() else {
        return Check::fail("GPU", "the NVIDIA driver works but reports no GPUs");
    };
    let driver = first
        .driver
        .as_deref()
        .map(|d| format!(" · driver {d}"))
        .unwrap_or_default();
    let count = match info.len() {
        1 => first.name.clone(),
        n => format!("{n} GPUs ({})", first.name),
    };
    Check::ok("GPU", format!("{count}{driver}"))
}

/// What the network allows, as phones will experience it, and how far each relay is.
async fn connection(config: &Config) -> Vec<Check> {
    let mut builder = Endpoint::builder(presets::N0).proxy_from_env();
    let relays: Vec<RelayUrl> = config
        .relays
        .iter()
        .filter_map(|u| u.parse().ok())
        .collect();
    if !relays.is_empty() {
        builder = builder.relay_mode(RelayMode::custom(relays));
    }
    let endpoint = match builder.bind().await {
        Ok(e) => e,
        Err(e) => {
            return vec![Check::fail(
                "Network",
                format!("could not open a socket: {e}"),
            )];
        }
    };
    let report = tokio::time::timeout(NET_REPORT_TIMEOUT, endpoint.net_report().initialized())
        .await
        .ok();
    endpoint.close().await;
    match report {
        Some(r) => {
            let (ok, detail) = classify(
                r.udp_v4 || r.udp_v6,
                r.mapping_varies_by_dest(),
                r.preferred_relay.is_some(),
            );
            let mut best: Vec<(String, Duration)> = Vec::new();
            for (_, url, latency) in r.relay_latency.iter() {
                let host = url.host_str().unwrap_or("relay").to_string();
                match best.iter_mut().find(|(h, _)| *h == host) {
                    Some((_, l)) => *l = (*l).min(latency),
                    None => best.push((host, latency)),
                }
            }
            let preferred = r
                .preferred_relay
                .as_ref()
                .and_then(|u| u.host_str())
                .map(String::from);
            vec![
                Check {
                    check: "Network",
                    ok,
                    detail,
                },
                Check::ok("Relays", relays_detail(&best, preferred.as_deref())),
            ]
        }
        None => vec![Check::fail(
            "Network",
            "no answer from the relays in 10 s. Is the machine online? A firewall or proxy may block them",
        )],
    }
}

/// A relay further than this adds a noticeable wait to relayed connections.
const FAR_MS: u128 = 150;

/// Each relay's round trip from here, nearest first, like
/// `relay.pocketgpu.app 38 ms (used) · use1-1.relay.n0.iroh.link 120 ms`.
pub fn relays_detail(latencies: &[(String, Duration)], preferred: Option<&str>) -> String {
    if latencies.is_empty() {
        return "no relay answered".into();
    }
    let mut sorted = latencies.to_vec();
    sorted.sort_by_key(|(_, l)| *l);
    let list = sorted
        .iter()
        .map(|(host, l)| {
            let used = if Some(host.as_str()) == preferred {
                " (used)"
            } else {
                ""
            };
            format!("{host} {} ms{used}", l.as_millis())
        })
        .collect::<Vec<_>>()
        .join(" · ");
    if sorted[0].1.as_millis() > FAR_MS {
        format!(
            "{list}. All relays are far: a relay nearer this machine would speed up relayed connections"
        )
    } else {
        list
    }
}

/// How phones will reach this machine, from the network report.
pub fn classify(udp: bool, mapping_varies: Option<bool>, relay: bool) -> (bool, String) {
    match (udp, mapping_varies, relay) {
        (false, _, false) => (
            false,
            "can't reach the relays and UDP is blocked, so phones can't connect. Check the firewall"
                .into(),
        ),
        (false, _, true) => (
            true,
            "relayed: UDP is blocked, so phones connect through a relay".into(),
        ),
        (true, Some(true), _) => (
            true,
            "mostly relayed: this network changes ports per destination (symmetric NAT)".into(),
        ),
        (true, _, _) => (true, "direct connections possible".into()),
    }
}

async fn ollama(config: &Config) -> Check {
    match ollama::version(&config.ollama_addr).await {
        // Whatever answers on that port wrote this: no control characters into the terminal.
        Some(v) => Check::ok("Ollama", format!("running {}", crate::audit::clean(&v))),
        // Ollama is optional, so its absence is not a failure.
        None => Check::ok(
            "Ollama",
            format!("not running at {} (optional)", config.ollama_addr),
        ),
    }
}

#[cfg(windows)]
fn disk(data_dir: &Path) -> Check {
    match crate::winsys::free_space(&existing_ancestor(data_dir)) {
        Some((free, drive)) => disk_check(free, &drive),
        None => Check::ok("Disk", "could not tell how much space is free"),
    }
}

#[cfg(not(windows))]
fn disk(data_dir: &Path) -> Check {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    // The disk holding the data folder is the one with the longest matching mount point.
    let dir = existing_ancestor(data_dir);
    let Some(d) = disks
        .iter()
        .filter(|d| dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
    else {
        return Check::ok("Disk", "could not tell which disk holds the data folder");
    };
    disk_check(d.available_space(), &d.mount_point().display().to_string())
}

fn disk_check(free: u64, mount: &str) -> Check {
    let free_gb = free as f64 / 1024f64.powi(3);
    let detail = format!("{free_gb:.1} GB free on {mount}");
    if free_gb < MIN_FREE_GB {
        Check::fail(
            "Disk",
            format!("{detail}. Free some space for logs and history"),
        )
    } else {
        Check::ok("Disk", detail)
    }
}

fn existing_ancestor(path: &Path) -> std::path::PathBuf {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            break;
        }
    }
    p.canonicalize().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relays_nearest_first() {
        let l = vec![
            ("far.example".to_string(), Duration::from_millis(210)),
            ("near.example".to_string(), Duration::from_millis(38)),
        ];
        assert_eq!(
            relays_detail(&l, Some("near.example")),
            "near.example 38 ms (used) · far.example 210 ms"
        );
        assert!(relays_detail(&l[..1], None).contains("All relays are far"));
        assert_eq!(relays_detail(&[], None), "no relay answered");
    }

    #[test]
    fn network_classes() {
        assert!(!classify(false, None, false).0);
        assert!(classify(false, None, true).1.starts_with("relayed"));
        assert!(
            classify(true, Some(true), true)
                .1
                .starts_with("mostly relayed")
        );
        assert_eq!(
            classify(true, Some(false), true).1,
            "direct connections possible"
        );
    }
}
