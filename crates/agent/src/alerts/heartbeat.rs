//! Heartbeats (T064): every 60 seconds the agent tells the push server it's up, so phones
//! hear when it goes silent; a clean stop says so, so they don't.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use pocketgpu_proto::push::{HEARTBEAT_EVERY_S, Heartbeat};

use super::client;
use crate::daemon::state::Agent;
use crate::store::devices;

/// Heartbeats are retried briefly; the next one comes within a minute anyway.
const RETRIES: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(10)];

/// Runs until the agent stops. Nothing is sent while no phone has a push ticket.
pub async fn run(agent: Arc<Agent>) {
    let Ok(http) = client::http() else { return };
    let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_EVERY_S));
    loop {
        tick.tick().await;
        if let Err(e) = beat(&agent, &http).await {
            tracing::debug!("heartbeat not sent: {e:#}");
        }
    }
}

async fn beat(agent: &Agent, http: &reqwest::Client) -> Result<()> {
    let tickets = devices::push_tickets(&agent.db())?;
    if tickets.is_empty() {
        return Ok(());
    }
    let (minutes, url) = {
        let cfg = agent.config.read().expect("config lock poisoned");
        (cfg.alerts.offline_after_min, cfg.push_url.clone())
    };
    // Offline alerts off: no heartbeats, so nothing is ever reported.
    if minutes <= 0.0 {
        return Ok(());
    }
    let hb = Heartbeat {
        offline_after_s: (minutes * 60.0).round().max(0.0) as u32,
        tickets,
    };
    client::signed_post(
        agent,
        http,
        &url,
        "/v1/heartbeat",
        serde_json::to_vec(&hb)?,
        &RETRIES,
    )
    .await?;
    Ok(())
}

/// On a clean stop: no "offline" push for this machine. Gives up after a few seconds.
pub async fn ended(agent: &Agent) {
    if devices::push_tickets(&agent.db()).map_or(true, |t| t.is_empty()) {
        return;
    }
    let url = agent
        .config
        .read()
        .expect("config lock poisoned")
        .push_url
        .clone();
    let Ok(http) = client::http() else { return };
    let send = client::signed_post(agent, &http, &url, "/v1/ended", b"{}".to_vec(), &[]);
    match tokio::time::timeout(Duration::from_secs(3), send).await {
        Ok(Ok(_)) => tracing::info!("told the push server this is a clean stop"),
        _ => tracing::warn!("couldn't reach the push server; phones may get an offline alert"),
    }
}
