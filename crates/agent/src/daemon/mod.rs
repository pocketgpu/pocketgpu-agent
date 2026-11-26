//! The long-running agent: sampler, network endpoint and local socket.

pub mod auth;
pub mod control;
pub mod crash;
pub mod devices;
pub mod endpoint;
pub mod events;
pub mod history;
pub mod metrics;
pub mod processes;
pub mod root;
pub mod state;
pub mod update;

use std::sync::Arc;

use anyhow::Result;

use crate::collect;
use crate::store::paths::Paths;
use endpoint::Relays;
use state::Agent;

/// Runs the agent in the foreground until Ctrl-C or a termination signal.
pub async fn run(paths: Paths) -> Result<()> {
    root::check()?;
    // A second agent for the same user would take over the local socket (Linux, macOS) and
    // leave the running one unreachable: `pgpu status` would then say it isn't running. So
    // stop here, before this one's endpoint or alerts start.
    if crate::ipc::client::connect(&paths).await.is_ok() {
        anyhow::bail!(ALREADY_RUNNING);
    }
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
    let _ = paths.create_all();
    // Two agents with one key and database fight over both, even when their sockets differ.
    let _lock = hold_lock(&paths)?;
    crash::install_hook(&paths);
    #[cfg(windows)]
    let data_dir = paths.data_dir.clone();
    let agent = Agent::open(paths, collect::from_env())?;
    // The PocketGPU app on this PC follows the agent's version (board 29).
    #[cfg(windows)]
    tokio::spawn(crate::desktop_app::keep_in_step(data_dir));
    #[cfg(windows)]
    if let Err(e) = crate::winsys::migrate_login_entry() {
        tracing::warn!("could not rename the login entry: {e:#}");
    }
    // An update leaves the old agent's systemd unit, which kills phones' tmux and nohup jobs.
    #[cfg(not(any(windows, target_os = "macos")))]
    tokio::task::spawn_blocking(crate::cli::service::upgrade_unit);
    // The last run's crash, if it crashed and crash reports are on (board 25).
    tokio::spawn({
        let agent = agent.clone();
        async move { crash::send_pending(&agent).await }
    });
    let endpoint = endpoint::bind(&agent, Relays::FromConfig).await?;
    tracing::info!(
        id = %agent.identity.agent_id(),
        name = %agent.name(),
        "agent started"
    );
    // Jobs still running from before get a while to reconnect (`pgpu run` keeps trying).
    tokio::spawn(crate::jobs::lose_unclaimed(
        agent.clone(),
        crate::jobs::RESUME_WITHIN,
    ));
    let router = endpoint::serve(agent.clone(), endpoint);
    let lan = tokio::spawn(crate::lan::announce(
        agent.clone(),
        router.endpoint().bound_sockets(),
    ));
    let vram = tokio::spawn(crate::jobs::track_vram(agent.clone()));
    let prune = tokio::spawn(crate::jobs::prune_hourly(agent.clone()));
    let files = tokio::spawn(crate::files::tidy_hourly(agent.clone()));
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
    crate::notices::start(agent.clone());
    // Ollama a phone set up here starts with the agent (board 87).
    crate::ai::setup::resume(agent.clone());
    // Guests whose time is up lose their access (F44).
    crate::pair::guest::start_sweeper(agent.clone());
    crate::share::start_sweeper(agent.clone());
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
    files.abort();
    alerts.abort();
    heartbeat.abort();
    rules.abort();
    lan.abort();
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

const ALREADY_RUNNING: &str = "PocketGPU is already running in the background for this user \
     (see `pgpu status`), so there's no need to start it here";

/// The running agent's hold on [`Paths::lock_file`]; the system lets go when it exits, even
/// by a crash. Where locks can't be taken at all (some network drives), the agent goes on.
fn hold_lock(paths: &Paths) -> Result<Option<std::fs::File>> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = match options.open(paths.lock_file()) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!("could not open {}: {e}", paths.lock_file().display());
            return Ok(None);
        }
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(ALREADY_RUNNING),
        Err(std::fs::TryLockError::Error(e)) => {
            tracing::warn!("could not lock {}: {e}", paths.lock_file().display());
            Ok(None)
        }
    }
}

/// Convenience for tests: an agent whose router serves on a local-only endpoint.
pub async fn spawn_local(agent: Arc<Agent>) -> Result<iroh::protocol::Router> {
    let endpoint = endpoint::bind(&agent, Relays::LocalOnly).await?;
    Ok(endpoint::serve(agent, endpoint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_agent_for_the_same_data_does_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path().to_path_buf());
        let first = hold_lock(&paths).unwrap();
        assert!(first.is_some());
        // Its socket elsewhere, the second agent still shares the data folder.
        let mut second = paths.clone();
        second.runtime_dir = dir.path().join("run");
        let err = hold_lock(&second).unwrap_err();
        assert!(err.to_string().contains("already running"), "{err:#}");
        drop(first);
        assert!(hold_lock(&second).unwrap().is_some());
    }
}
