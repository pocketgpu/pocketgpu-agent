//! The agent's crash reports (board 25): a panic writes a report next to the database; the
//! next start sends it when crash reports are on (`pgpu crash-reports on`) and drops it
//! otherwise. The install number is random, kept in `crash-id`, and is not the agent's key.

use std::time::Duration;

use pocketgpu_proto::crash::{self, CrashReport};

use super::state::Agent;
use crate::store::paths::Paths;

fn pending(paths: &Paths) -> std::path::PathBuf {
    paths.data_dir.join("crash.json")
}

/// A random 128-bit number in hex, made once.
fn install_id(paths: &Paths) -> String {
    let file = paths.data_dir.join("crash-id");
    if let Ok(id) = std::fs::read_to_string(&file)
        && id.trim().len() == 32
    {
        return id.trim().to_string();
    }
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return String::new();
    }
    let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let _ = std::fs::write(&file, &id);
    id
}

/// Called first thing at start.
pub fn install_hook(paths: &Paths) {
    crash::install_hook(
        pending(paths),
        CrashReport {
            install: install_id(paths),
            source: "agent".into(),
            platform: crash::platform().into(),
            version: env!("CARGO_PKG_VERSION").into(),
            os: Some(crate::collect::system::os_name()),
            model: None,
            message: String::new(),
            frames: Vec::new(),
            on_start: false,
        },
    );
}

/// Sends what the last run left, if crash reports are on; quiet on failure.
pub async fn send_pending(agent: &Agent) {
    let Some(report) = crash::take(&pending(&agent.paths)) else {
        return;
    };
    let (on, url) = {
        let cfg = agent.config.read().expect("config lock poisoned");
        (cfg.crash_reports, cfg.push_url.clone())
    };
    if !on {
        return;
    }
    let sent = async {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?
            .post(format!("{url}/v1/crash"))
            .json(&report)
            .send()
            .await?
            .error_for_status()
    }
    .await;
    match sent {
        Ok(_) => tracing::info!("sent the report of the last crash"),
        Err(e) => tracing::warn!("couldn't send the last crash report: {e}"),
    }
}
