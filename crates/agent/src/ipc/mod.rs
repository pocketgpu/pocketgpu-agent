//! Local socket between the command line and the agent. Unix socket (mode 0600) on Linux,
//! a named pipe on Windows. Never reachable from the network.

use std::sync::Arc;

use anyhow::Result;
use pocketgpu_proto::local::{Request, Response, Status};
use pocketgpu_proto::{read_frame, write_frame};
use rusqlite::OptionalExtension;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::daemon::state::Agent;

pub mod client;

/// Reads frames on a task of their own, so waiting for them can be cancelled safely.
pub fn forward_frames<R, T>(mut reader: R) -> tokio::sync::mpsc::Receiver<T>
where
    R: AsyncRead + Unpin + Send + 'static,
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        while let Ok(Some(msg)) = read_frame::<_, T>(&mut reader).await {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    });
    rx
}

async fn serve_stream<S>(agent: Arc<Agent>, stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut r, mut w) = tokio::io::split(stream);
    while let Ok(Some(req)) = read_frame::<_, Request>(&mut r).await {
        // Pairing and jobs take over this stream until they end.
        match req {
            Request::Pair => {
                if let Err(e) = crate::pair::session(agent.clone(), r, w).await {
                    tracing::warn!("pairing failed: {e:#}");
                }
                return;
            }
            Request::JobStart(start) => {
                if let Err(e) = crate::jobs::session(agent.clone(), start, r, w).await {
                    tracing::warn!("job session failed: {e:#}");
                }
                return;
            }
            Request::GetDocker | Request::SetDocker { .. } => {
                let on = match req {
                    Request::SetDocker { on } => Some(on),
                    _ => None,
                };
                let reply = crate::docker::local(&agent, on).await;
                if write_frame(&mut w, &reply).await.is_err() {
                    break;
                }
                continue;
            }
            Request::TestChannels => {
                let alert = crate::alerts::channels::Alert {
                    title: format!("Test alert · {}", agent.name()),
                    body: "PocketGPU alerts will arrive here.".into(),
                    kind: "test",
                    machine: agent.name(),
                    job: None,
                };
                let results = crate::alerts::channels::send_all(&agent, &alert).await;
                if write_frame(&mut w, &Response::ChannelTest(results))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            _ => {}
        }
        let reply = handle(&agent, req);
        if write_frame(&mut w, &reply).await.is_err() {
            break;
        }
    }
}

pub fn handle(agent: &Arc<Agent>, req: Request) -> Response {
    match req {
        Request::Status => Response::Status(Box::new(status(agent))),
        Request::Pair | Request::PairDecision { .. } => {
            Response::Error("no pairing is in progress on this connection".into())
        }
        Request::ListJobs { limit } => match crate::jobs::store::list(&agent.db(), limit, None) {
            Ok(jobs) => Response::Jobs(jobs),
            Err(e) => Response::Error(format!("{e:#}")),
        },
        Request::GetJob { id } => match crate::jobs::store::get(&agent.db(), id) {
            Ok(Some((detail, log))) => Response::Job {
                detail: Box::new(detail),
                log_path: log.to_string_lossy().into_owned(),
            },
            Ok(None) => Response::Error(format!("no job {id}")),
            Err(e) => Response::Error(format!("{e:#}")),
        },
        Request::StopJob { id } => match agent.jobs.stop(agent, id) {
            Ok(()) => {
                let _ = crate::audit::record(
                    &agent.db(),
                    "local",
                    crate::audit::Event::JobStop,
                    &format!("job {id}"),
                    "ok",
                );
                Response::Ok
            }
            Err(e) => Response::Error(e.to_string()),
        },
        Request::GetAlerts => Response::Alerts(crate::alerts::settings::get(agent)),
        Request::SetAlerts(rules) => match crate::alerts::settings::set(agent, &rules) {
            Ok(rules) => Response::Alerts(rules),
            Err(e) => Response::Error(format!("{e:#}")),
        },
        Request::ListActions => crate::actions::local::list(agent),
        Request::AddAction(a) => crate::actions::local::add(agent, &a),
        Request::EditAction(e) => crate::actions::local::edit(agent, &e),
        Request::RemoveAction { action } => crate::actions::local::remove(agent, &action),
        Request::Audit { since_ms } => {
            let entries = crate::audit::named(&agent.db(), since_ms);
            match entries {
                Ok(e) => Response::Audit(e),
                Err(e) => Response::Error(format!("{e:#}")),
            }
        }
        Request::ListDevices => match crate::daemon::devices::list(agent) {
            Ok(d) => Response::Devices(d),
            Err(e) => Response::Error(format!("{e:#}")),
        },
        Request::SetDevicePermission { device, permission } => {
            match crate::daemon::devices::set_permission(agent, &device, permission) {
                Ok(d) => Response::Device(d),
                Err(e) => Response::Error(e),
            }
        }
        Request::RevokeDevice { device } => match crate::daemon::devices::revoke(agent, &device) {
            Ok(d) => Response::Device(d),
            Err(e) => Response::Error(e),
        },
        Request::PauseAlerts { minutes } => {
            let until = match minutes {
                0 => 0,
                m => crate::store::db::now_ms() + i64::from(m.min(7 * 24 * 60)) * 60_000,
            };
            agent
                .alerts_paused_until
                .store(until, std::sync::atomic::Ordering::Relaxed);
            let what = match minutes {
                0 => "resumed".to_string(),
                m => format!("paused for {m} min"),
            };
            let _ = crate::audit::record(
                &agent.db(),
                "local",
                crate::audit::Event::AlertSettings,
                &what,
                "ok",
            );
            Response::Status(Box::new(status(agent)))
        }
        Request::Shutdown => {
            agent.stop.notify_one();
            Response::Ok
        }
        Request::GetShell => shell_state(agent),
        Request::SetShell { on } => match set_shell(agent, on) {
            Ok(()) => shell_state(agent),
            Err(e) => Response::Error(format!("{e:#}")),
        },
        Request::ListChannels => channels(agent),
        Request::GetRelays => Response::Relays(
            agent
                .config
                .read()
                .expect("config lock poisoned")
                .relays
                .clone(),
        ),
        Request::SetRelays(urls) => match set_relays(agent, urls) {
            Ok(list) => Response::Relays(list),
            Err(e) => Response::Error(format!("{e:#}")),
        },
        Request::AddChannel(c) => match change_channels(agent, |list| {
            crate::alerts::channels::validate(&c)?;
            if list.len() >= crate::alerts::channels::MAX {
                anyhow::bail!("at most {} channels", crate::alerts::channels::MAX);
            }
            if list.contains(&c) {
                anyhow::bail!("that channel is already set up");
            }
            list.push(c);
            Ok(())
        }) {
            Ok(()) => channels(agent),
            Err(e) => Response::Error(format!("{e:#}")),
        },
        Request::RemoveChannel { number } => match change_channels(agent, |list| {
            let i = (number as usize).checked_sub(1).filter(|i| *i < list.len());
            let Some(i) = i else {
                anyhow::bail!("there is no channel {number}; see pgpu notify");
            };
            list.remove(i);
            Ok(())
        }) {
            Ok(()) => channels(agent),
            Err(e) => Response::Error(format!("{e:#}")),
        },
        Request::JobStart(_) | Request::JobProcess { .. } | Request::JobEnd { .. } => {
            Response::Error("no job is running on this connection".into())
        }
        Request::TestChannels => Response::Error("channel tests run on the stream handler".into()),
        Request::GetDocker | Request::SetDocker { .. } => {
            Response::Error("Docker requests run on the stream handler".into())
        }
    }
}

/// Saves custom relays after checking each URL; the endpoint picks them up on restart.
fn set_relays(agent: &Agent, urls: Vec<String>) -> Result<Vec<String>> {
    let mut list = Vec::new();
    for u in urls {
        let u = u.trim().to_string();
        if !u.starts_with("https://") {
            anyhow::bail!("{u}: a relay URL starts with https://");
        }
        u.parse::<iroh::RelayUrl>()
            .map_err(|e| anyhow::anyhow!("{u} is not a valid relay URL: {e}"))?;
        if !list.contains(&u) {
            list.push(u);
        }
    }
    if list.len() > 5 {
        anyhow::bail!("at most 5 relays");
    }
    let mut cfg = agent.config.write().expect("config lock poisoned");
    let mut next = cfg.clone();
    next.relays = list.clone();
    next.save(&agent.paths.config_file())?;
    *cfg = next;
    drop(cfg);
    let _ = crate::audit::record(
        &agent.db(),
        "local",
        crate::audit::Event::RelayChange,
        "relays",
        &list.join(" "),
    );
    Ok(list)
}

fn channels(agent: &Agent) -> Response {
    let cfg = agent.config.read().expect("config lock poisoned");
    Response::Channels(
        cfg.notify
            .iter()
            .map(crate::alerts::channels::describe)
            .collect(),
    )
}

/// Changes the channel list and saves `config.toml`.
fn change_channels(
    agent: &Agent,
    change: impl FnOnce(&mut Vec<pocketgpu_proto::local::Channel>) -> Result<()>,
) -> Result<()> {
    let mut cfg = agent.config.write().expect("config lock poisoned");
    let mut next = cfg.clone();
    change(&mut next.notify)?;
    next.save(&agent.paths.config_file())?;
    *cfg = next;
    Ok(())
}

fn shell_state(agent: &Agent) -> Response {
    Response::Shell {
        enabled: agent.config.read().expect("config lock poisoned").shell,
        sessions: agent.shells.count() as u32,
    }
}

/// `pgpu shell on|off`: saved in `config.toml`; off closes every session at once.
fn set_shell(agent: &Arc<Agent>, on: bool) -> Result<()> {
    {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        let mut next = cfg.clone();
        next.shell = on;
        next.save(&agent.paths.config_file())?;
        *cfg = next;
    }
    if !on {
        agent
            .shells
            .close_all(agent, "closed: terminal access turned off");
    }
    let state = if on { "on" } else { "off" };
    let _ = crate::audit::record(
        &agent.db(),
        "local",
        crate::audit::Event::ShellAccess,
        "terminal",
        state,
    );
    Ok(())
}

pub fn status(agent: &Agent) -> Status {
    let paired: u32 = agent
        .db()
        .query_row("SELECT count(*) FROM devices", [], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
        .unwrap_or(0);
    let gpus = agent.sampler.gpus();
    Status {
        name: agent.name(),
        agent_id: agent.identity.agent_id().to_string(),
        fingerprint: agent.identity.fingerprint(),
        version: env!("CARGO_PKG_VERSION").into(),
        gpus: gpus.info(),
        latest: agent.sampler.latest(),
        gpu_problem: gpus.problem(),
        paired_devices: paired,
        connected_devices: agent.connected_devices(),
        direct_devices: agent.direct_devices(),
        started_ms: agent.started_ms,
        alerts_paused_until_ms: crate::alerts::paused_until(agent),
    }
}

#[cfg(unix)]
pub async fn listen(agent: Arc<Agent>) -> Result<()> {
    use anyhow::Context;
    let path = agent.paths.socket();
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    crate::store::paths::set_private(&path)?;
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(serve_stream(agent.clone(), stream));
            }
            // Out of file descriptors, say: the socket itself is fine, so wait and go on.
            Err(e) => {
                tracing::warn!("local socket accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

#[cfg(windows)]
pub async fn listen(agent: Arc<Agent>) -> Result<()> {
    use anyhow::Context;
    use pocketgpu_client::local::winpipe::PipeSecurity;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    ensure_pipe_tag(&agent.paths)?;
    let name = client::pipe_name();
    // Only this user (and SYSTEM) may open the pipe, whatever the default DACL allows.
    let mut security =
        PipeSecurity::for_current_user().context("could not build the pipe's permissions")?;
    let mut create = |first: bool| -> std::io::Result<NamedPipeServer> {
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        // SAFETY: the attributes and their descriptor live in `security` for this call.
        unsafe { options.create_with_security_attributes_raw(&name, security.as_raw()) }
    };
    // Windows refuses a first instance with "Access is denied" when the pipe already exists.
    let mut server = match create(true) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => anyhow::bail!(
            "another PocketGPU agent is already running for this user (see `pgpu status`); \
             stop it before starting a second one"
        ),
        other => other?,
    };
    loop {
        // A client that went away mid-connect fails only its own instance.
        if let Err(e) = server.connect().await {
            tracing::warn!("local pipe connect failed: {e}");
            server = create(false)?;
            continue;
        }
        let connected = server;
        server = create(false)?;
        tokio::spawn(serve_stream(agent.clone(), connected));
    }
}

/// Writes the random part of the pipe name once (see `pocketgpu_client::local::pipe_name`).
#[cfg(windows)]
fn ensure_pipe_tag(paths: &crate::store::paths::Paths) -> Result<()> {
    let file = paths.data_dir.join(pocketgpu_client::local::PIPE_TAG_FILE);
    if file.exists() {
        return Ok(());
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no random numbers: {e}"))?;
    let tag: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    crate::store::paths::write_private(&file, tag.as_bytes())
}
