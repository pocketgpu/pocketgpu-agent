//! Watched jobs: `pgpu run` runs the command and reports to the agent over the local socket;
//! the agent records the job, serves it to phones, relays stop requests and tracks VRAM.

pub mod handler;
#[cfg(unix)]
pub mod pty;
pub mod runner;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use pocketgpu_proto::local::{JobStart, Request, Response};
use pocketgpu_proto::msg::jobs::JobState;
use pocketgpu_proto::write_frame;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc};

use crate::daemon::state::Agent;

/// How long a stopped job gets between SIGTERM and SIGKILL.
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// Sent when a job ends, for alerts (F08).
#[derive(Debug, Clone, PartialEq)]
pub struct JobEnded {
    pub id: u64,
    pub name: String,
    pub state: JobState,
    pub exit_code: Option<i32>,
    pub alerts_on: bool,
}

/// The jobs whose `pgpu run` is connected right now.
#[derive(Debug)]
pub struct Jobs {
    running: Mutex<HashMap<u64, Running>>,
    ended: broadcast::Sender<JobEnded>,
}

#[derive(Debug)]
struct Running {
    stop: mpsc::Sender<()>,
    pgid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StopError {
    #[error("no job with that number")]
    NotFound,
    #[error("the job is not running")]
    NotRunning,
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
            ended: broadcast::channel(64).0,
        }
    }
}

impl Jobs {
    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Running>> {
        self.running.lock().expect("jobs lock poisoned")
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobEnded> {
        self.ended.subscribe()
    }

    pub fn is_running(&self, id: u64) -> bool {
        self.running().contains_key(&id)
    }

    /// Asks the job's `pgpu run` to stop it: SIGTERM, then SIGKILL after 10 seconds.
    pub fn stop(&self, agent: &Agent, id: u64) -> Result<(), StopError> {
        if let Some(r) = self.running().get(&id) {
            let _ = r.stop.try_send(());
            return Ok(());
        }
        match store::get(&agent.db(), id) {
            Ok(Some(_)) => Err(StopError::NotRunning),
            _ => Err(StopError::NotFound),
        }
    }

    /// Process groups of running jobs, for VRAM tracking.
    fn groups(&self) -> Vec<(u64, u32)> {
        self.running()
            .iter()
            .filter_map(|(id, r)| r.pgid.map(|g| (*id, g)))
            .collect()
    }
}

/// How long jobs that were running when the agent stopped get to come back (`JobResume`)
/// before they count as lost; `pgpu run` keeps trying as long.
pub const RESUME_WITHIN: Duration = Duration::from_secs(5 * 60);

/// One `pgpu run` from `JobStart` to `JobEnd`. A stream that closes first marks the job lost.
pub async fn session<R, W>(agent: Arc<Agent>, start: JobStart, reader: R, mut w: W) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let created = store::create(&agent.db(), &start, &agent.paths.jobs_dir());
    let (id, log) = match created {
        Ok(v) => v,
        Err(e) => {
            write_frame(
                &mut w,
                &Response::Error(format!("could not record the job: {e:#}")),
            )
            .await?;
            return Ok(());
        }
    };
    tracing::info!(job = id, name = %start.name, "job started");
    watch(agent, id, None, start.name, start.alerts, &log, reader, w).await
}

/// `pgpu run` again after the agent restarted: the job kept running and writing its log, so
/// it carries on where it was, alerts and all.
pub async fn resume<R, W>(
    agent: Arc<Agent>,
    id: u64,
    pgid: Option<u32>,
    reader: R,
    mut w: W,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let found = store::get(&agent.db(), id).ok().flatten();
    let Some((detail, log)) =
        found.filter(|(d, _)| d.summary.state == JobState::Running && !agent.jobs.is_running(id))
    else {
        write_frame(
            &mut w,
            &Response::Error(format!("job {id} isn't waiting to be picked up")),
        )
        .await?;
        return Ok(());
    };
    tracing::info!(job = id, "job picked up again after a restart");
    let alerts = detail.alerts_on;
    watch(
        agent,
        id,
        pgid,
        detail.summary.name,
        alerts,
        &log,
        reader,
        w,
    )
    .await
}

/// The rest of a job's session, from `JobStarted` until it ends or its `pgpu run` goes away.
#[allow(clippy::too_many_arguments)]
async fn watch<R, W>(
    agent: Arc<Agent>,
    id: u64,
    pgid: Option<u32>,
    name: String,
    alerts: bool,
    log: &std::path::Path,
    reader: R,
    mut w: W,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let (stop_tx, mut stop_rx) = mpsc::channel(1);
    agent.jobs.running().insert(
        id,
        Running {
            stop: stop_tx,
            pgid,
        },
    );
    write_frame(
        &mut w,
        &Response::JobStarted {
            id,
            log_path: log.to_string_lossy().into_owned(),
        },
    )
    .await?;

    let mut from_wrapper = crate::ipc::forward_frames::<_, Request>(reader);
    let state = loop {
        tokio::select! {
            msg = from_wrapper.recv() => match msg {
                Some(Request::JobProcess { pgid }) => {
                    if let Some(r) = agent.jobs.running().get_mut(&id) {
                        r.pgid = Some(pgid);
                    }
                }
                Some(Request::JobEnd { exit_code, stopped }) => {
                    let state = store::end(&agent.db(), id, exit_code, stopped)?;
                    let _ = write_frame(&mut w, &Response::Ok).await;
                    break (state, exit_code);
                }
                Some(_) => {}
                None => {
                    store::mark_lost(&agent.db(), id)?;
                    break (JobState::Lost, None);
                }
            },
            Some(()) = stop_rx.recv() => {
                tracing::info!(job = id, "stop requested");
                let _ = write_frame(&mut w, &Response::StopJob).await;
            }
        }
    };
    agent.jobs.running().remove(&id);
    tracing::info!(job = id, state = ?state.0, "job ended");
    // As set now: a phone may have turned the job's alerts off (or on) while it ran.
    let alerts_on = store::get(&agent.db(), id)
        .ok()
        .flatten()
        .map_or(alerts, |(detail, _)| detail.alerts_on);
    let _ = agent.jobs.ended.send(JobEnded {
        id,
        name,
        state: state.0,
        exit_code: state.1,
        alerts_on,
    });
    Ok(())
}

/// At start: jobs the last run left as running get [`RESUME_WITHIN`] to be picked up again
/// by their `pgpu run`; the ones that aren't are marked lost then.
pub async fn lose_unclaimed(agent: Arc<Agent>, after: Duration) {
    let waiting = match store::running_ids(&agent.db()) {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("could not check old jobs: {e:#}");
            return;
        }
    };
    if waiting.is_empty() {
        return;
    }
    tracing::info!(
        "{} job(s) were running when the agent stopped; waiting for them",
        waiting.len()
    );
    tokio::time::sleep(after).await;
    for id in waiting {
        if !agent.jobs.is_running(id) && store::mark_lost(&agent.db(), id).is_ok() {
            tracing::warn!(
                job = id,
                "job didn't come back after the restart; marked lost"
            );
        }
    }
}

/// Every few seconds: the VRAM of each running job's processes, kept as its peak.
pub async fn track_vram(agent: Arc<Agent>) {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let groups = agent.jobs.groups();
        if groups.is_empty() {
            continue;
        }
        let gpus = agent.sampler.gpus().clone();
        let procs = tokio::task::spawn_blocking(move || gpus.processes())
            .await
            .unwrap_or_default();
        for (id, pgid) in groups {
            let mb: u64 = procs
                .iter()
                .filter(|p| in_group(p.pid, pgid))
                .filter_map(|p| p.vram_mb)
                .sum();
            if mb > 0 {
                let _ = store::bump_peak_vram(&agent.db(), id, mb);
            }
        }
    }
}

/// Windows has no process groups: a job owns every process its first one started.
#[cfg(windows)]
fn in_group(pid: u32, root: u32) -> bool {
    crate::winsys::ancestors(pid).contains(&root)
}

#[cfg(not(windows))]
fn in_group(pid: u32, pgid: u32) -> bool {
    process_group(pid) == Some(pgid)
}

/// Hourly: old logs and jobs go (30 days, 1 GB of logs in total), action runs after 30 days
/// and audit entries after 90.
pub async fn prune_hourly(agent: Arc<Agent>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    loop {
        tick.tick().await;
        if let Err(e) = store::prune(&agent.db()) {
            tracing::warn!("could not prune old jobs: {e:#}");
        }
        let now = crate::store::db::now_ms();
        if let Err(e) = crate::actions::store::prune(&agent.db(), now) {
            tracing::warn!("could not prune old action runs: {e:#}");
        }
        if let Err(e) = crate::audit::prune(&agent.db()) {
            tracing::warn!("could not prune the audit log: {e:#}");
        }
    }
}

/// A process's group from the kernel's process info.
#[cfg(target_os = "macos")]
fn process_group(pid: u32) -> Option<u32> {
    // SAFETY: proc_pidinfo fills at most `size` bytes of the zeroed struct.
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        );
        (n == size).then_some(info.pbi_pgid)
    }
}

/// A process's group from `/proc/<pid>/stat`.
#[cfg(not(any(windows, target_os = "macos")))]
fn process_group(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces, so count fields after its closing parenthesis.
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split(' ').nth(2)?.parse().ok()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn reads_own_process_group() {
        // SAFETY: getpgrp has no preconditions.
        let mine = unsafe { libc::getpgrp() } as u32;
        assert_eq!(process_group(std::process::id()), Some(mine));
    }
}
