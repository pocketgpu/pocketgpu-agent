//! The long-running agent: sampler, network endpoint and local socket.

pub mod auth;
pub mod control;
pub mod devices;
pub mod endpoint;
pub mod events;
pub mod history;
pub mod metrics;
pub mod processes;
pub mod root;
pub mod state;

use std::sync::Arc;

use anyhow::Result;

use crate::collect;
use crate::store::paths::Paths;
use endpoint::Relays;
use state::Agent;

/// Runs the agent in the foreground until Ctrl-C or a termination signal.
pub async fn run(paths: Paths) -> Result<()> {
    root::check()?;
    // launchd holds the log open (append mode), so it is emptied in place when too big.
    #[cfg(target_os = "macos")]
    if let Ok(log) = crate::cli::service::log_path() {
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .and_then(|f| match f.metadata() {
                Ok(m) if m.len() > 10 * 1024 * 1024 => f.set_len(0),
                _ => Ok(()),
            });
    }
    let agent = Agent::open(paths, collect::from_env())?;
    let endpoint = endpoint::bind(&agent, Relays::FromConfig).await?;
    tracing::info!(
        id = %agent.identity.agent_id(),
        name = %agent.name(),
        "agent started"
    );
    match crate::jobs::store::lose_running(&agent.db()) {
        Ok(0) => {}
        Ok(n) => tracing::warn!("{n} job(s) were running when the agent stopped; marked lost"),
        Err(e) => tracing::warn!("could not check old jobs: {e:#}"),
    }
    let router = endpoint::serve(agent.clone(), endpoint);
    let vram = tokio::spawn(crate::jobs::track_vram(agent.clone()));
    let prune = tokio::spawn(crate::jobs::prune_hourly(agent.clone()));
    let alerts = tokio::spawn(crate::alerts::sender::run(agent.clone()));
    let heartbeat = tokio::spawn(crate::alerts::heartbeat::run(agent.clone()));
    let rules = tokio::spawn(crate::alerts::rules::run(agent.clone()));
    let ollama_addr = agent
        .config
        .read()
        .expect("config lock poisoned")
        .ollama_addr
        .clone();
    let sampler = tokio::spawn(agent.sampler.clone().run(agent.db.clone(), ollama_addr));
    let ipc = tokio::spawn(crate::ipc::listen(agent.clone()));

    // A local socket that can't be opened is a failure: the service manager restarts the
    // agent, as it wouldn't after a clean exit.
    let mut failed = None;
    tokio::select! {
        _ = shutdown_signal() => tracing::info!("stopping"),
        _ = agent.stop.notified() => tracing::info!("stopping on request"),
        res = ipc => {
            let e = match res {
                Ok(Err(e)) => e,
                Ok(Ok(())) => anyhow::anyhow!("the local socket closed"),
                Err(e) => anyhow::anyhow!(e),
            };
            tracing::error!("local socket failed: {e:#}");
            failed = Some(e);
        }
    }
    sampler.abort();
    vram.abort();
    prune.abort();
    alerts.abort();
    heartbeat.abort();
    rules.abort();
    crate::alerts::heartbeat::ended(&agent).await;
    // Connected phones show "ended" rather than "offline".
    agent.close_all(pocketgpu_proto::ENDED_REASON);
    router.shutdown().await.ok();
    match failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("could not install signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Convenience for tests: an agent whose router serves on a local-only endpoint.
pub async fn spawn_local(agent: Arc<Agent>) -> Result<iroh::protocol::Router> {
    let endpoint = endpoint::bind(&agent, Relays::LocalOnly).await?;
    Ok(endpoint::serve(agent, endpoint))
}
