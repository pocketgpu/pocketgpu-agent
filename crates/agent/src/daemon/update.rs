//! Agent updates from a phone (board 23): `CheckUpdate` says whether a newer release is out,
//! `InstallUpdate` installs it the way `pgpu update` does (signed `SHA256SUMS`, checksum, a
//! test run of the new binary) and then restarts the agent through its service. The phone
//! only asks; everything is downloaded and checked here.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pocketgpu_proto::msg::control::{Response, UpdateCheck};
use pocketgpu_proto::{Error, ErrorCode};

use super::auth::Peer;
use super::state::Agent;
use crate::cli::update as cli;
use crate::release;

const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// How long the newest release found is reused. GitHub allows 60 unauthenticated lookups an
/// hour per address, and this is at most 4 per machine; a release shows on phones within
/// about half an hour (this, then the phone's own `FRESH_MS`).
const FRESH: Duration = Duration::from_secs(15 * 60);
/// Time for the reply to reach the phone before the agent goes away.
const BEFORE_RESTART: Duration = Duration::from_millis(1500);

/// The newest release tag and when it was looked up.
static LATEST: Mutex<Option<(Instant, String)>> = Mutex::new(None);
/// One install at a time.
static BUSY: AtomicBool = AtomicBool::new(false);

pub async fn check() -> Response {
    let latest = latest_tag()
        .await
        .inspect_err(|e| tracing::warn!("update check: {e:#}"))
        .ok();
    Response::Update(UpdateCheck {
        current: CURRENT.into(),
        available: latest
            .as_deref()
            .is_some_and(|t| release::is_newer(t, CURRENT)),
        latest: latest.map(|t| t.trim_start_matches('v').to_string()),
        // Asking the service manager runs a program.
        blocked: tokio::task::spawn_blocking(blocked).await.unwrap_or(None),
    })
}

/// Why this machine can't be updated from a phone, if it can't.
fn blocked() -> Option<String> {
    if crate::daemon::root::in_container() {
        return Some("This agent runs in a container: pull the new image instead".into());
    }
    if release::archive_name().is_none() {
        return Some(format!(
            "There are no releases for {} CPUs",
            std::env::consts::ARCH
        ));
    }
    if crate::cli::service::state() != "active" {
        return Some(
            "The agent isn't running as a service here, so it can't restart itself. Run pgpu update on the machine"
                .into(),
        );
    }
    None
}

async fn latest_tag() -> anyhow::Result<String> {
    if let Some((at, tag)) = LATEST.lock().expect("update lock poisoned").as_ref()
        && at.elapsed() < FRESH
    {
        return Ok(tag.clone());
    }
    let client = cli::client()?;
    let tag = cli::latest_tag(&client, &cli::repo()).await?;
    *LATEST.lock().expect("update lock poisoned") = Some((Instant::now(), tag.clone()));
    Ok(tag)
}

fn refuse(code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error(Error::new(code, message))
}

pub async fn install(agent: &std::sync::Arc<Agent>, peer: &Peer, version: &str) -> Response {
    let tag = format!("v{}", version.trim_start_matches('v'));
    if release::parse_version(&tag).is_none() {
        return refuse(
            ErrorCode::Invalid,
            format!("{version} is not a release version"),
        );
    }
    if let Some(why) = tokio::task::spawn_blocking(blocked).await.unwrap_or(None) {
        return refuse(ErrorCode::Busy, why);
    }
    if release::same_version(&tag, CURRENT) {
        return refuse(
            ErrorCode::Invalid,
            format!("This agent is already on {CURRENT}"),
        );
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return refuse(ErrorCode::Busy, "An update is already being installed");
    }
    let result = download_and_install(&tag).await;
    let outcome = match &result {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("failed: {e:#}"),
    };
    let _ = crate::audit::record(
        &agent.db(),
        &peer.key.to_string(),
        crate::audit::Event::Update,
        &tag,
        &outcome,
    );
    match result {
        Ok(()) => {
            tracing::info!("installed agent {tag} from a phone; restarting");
            restart_soon(agent.clone());
            Response::Installed {
                version: tag.trim_start_matches('v').into(),
                restarting: true,
            }
        }
        Err(e) => {
            BUSY.store(false, Ordering::SeqCst);
            refuse(
                ErrorCode::Internal,
                format!("{e:#}. The agent stays on {CURRENT}"),
            )
        }
    }
}

async fn download_and_install(tag: &str) -> anyhow::Result<()> {
    let client = cli::client()?;
    let binary = cli::fetch(&client, &cli::repo(), tag).await?;
    let exe = std::env::current_exe()?;
    let tag = tag.to_string();
    tokio::task::spawn_blocking(move || cli::install(&exe, &binary, &tag)).await?
}

/// Restarts through the service once the reply is out. systemd and launchd stop this
/// process and start the new binary; on Windows a detached helper waits for this agent to
/// stop, then starts the new one.
fn restart_soon(agent: std::sync::Arc<Agent>) {
    tokio::spawn(async move {
        tokio::time::sleep(BEFORE_RESTART).await;
        #[cfg(windows)]
        let res = crate::cli::service::restart_after_stop().map(|()| agent.stop.notify_one());
        #[cfg(not(windows))]
        let res = {
            drop(agent);
            tokio::task::spawn_blocking(crate::cli::service::restart)
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r)
        };
        if let Err(e) = res {
            tracing::error!("could not restart after the update: {e:#}");
            BUSY.store(false, Ordering::SeqCst);
        }
    });
}
