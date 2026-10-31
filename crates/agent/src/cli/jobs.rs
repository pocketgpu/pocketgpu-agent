//! `pgpu jobs`, `pgpu logs <job> [-f]` and `pgpu stop <job>`.

use std::io::Write;
use std::time::Duration;

use anyhow::{Result, bail};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::msg::jobs::{JobState, JobSummary};

use crate::ipc::client;
use crate::store::db::now_ms;
use crate::store::paths::Paths;

pub async fn list(paths: &Paths, json: bool, limit: u32) -> Result<()> {
    let jobs = match client::call(paths, Request::ListJobs { limit }).await? {
        Response::Jobs(j) => j,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&jobs)?);
    } else {
        print!("{}", render(&jobs, now_ms() as u64));
    }
    Ok(())
}

pub fn render(jobs: &[JobSummary], now_ms: u64) -> String {
    if jobs.is_empty() {
        return "No jobs yet. Start one with: pgpu run -- <command>\n".into();
    }
    let mut out = format!(
        "{:>4}  {:<9} {:<24} {:>9}  {}\n",
        "JOB", "STATE", "NAME", "TIME", "EXIT"
    );
    for j in jobs {
        let end = j.ended_ms.unwrap_or(now_ms);
        let time = short_duration(Duration::from_millis(end.saturating_sub(j.started_ms)));
        let exit = j.exit_code.map_or(String::new(), |c| c.to_string());
        let name: String = j.name.chars().take(24).collect();
        out += &format!(
            "{:>4}  {:<9} {:<24} {:>9}  {exit}\n",
            j.id,
            state_label(j.state),
            name,
            time
        );
    }
    out
}

pub(crate) fn state_label(s: JobState) -> &'static str {
    match s {
        JobState::Running => "running",
        JobState::Finished => "finished",
        JobState::Crashed => "crashed",
        JobState::Stopped => "stopped",
        JobState::Lost => "lost",
        JobState::Unknown => "unknown",
    }
}

fn short_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// Prints the job's log; with `follow`, keeps printing new output until the job ends.
pub async fn logs(paths: &Paths, id: u64, follow: bool) -> Result<()> {
    let (mut running, log) = job_state(paths, id).await?;
    let mut offset = 0u64;
    let mut stdout = std::io::stdout();
    loop {
        let chunk = read_from(&log, &mut offset);
        if !chunk.is_empty() {
            stdout.write_all(&chunk)?;
            stdout.flush()?;
            continue;
        }
        if !follow || !running {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        running = job_state(paths, id).await?.0;
    }
}

async fn job_state(paths: &Paths, id: u64) -> Result<(bool, std::path::PathBuf)> {
    match client::call(paths, Request::GetJob { id }).await? {
        Response::Job { detail, log_path } => {
            Ok((detail.summary.state == JobState::Running, log_path.into()))
        }
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
}

fn read_from(path: &std::path::Path, offset: &mut u64) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    // The log was cut down to its end: carry on from its new end.
    if len < *offset {
        *offset = len;
    }
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(*offset)).is_ok() {
        let _ = f.take(256 * 1024).read_to_end(&mut buf);
    }
    *offset += buf.len() as u64;
    buf
}

pub async fn stop(paths: &Paths, json: bool, id: u64) -> Result<()> {
    match client::call(paths, Request::StopJob { id }).await? {
        Response::Ok if json => println!("{}", serde_json::json!({ "ok": true })),
        Response::Ok => println!(
            "Stopping job {id}: SIGTERM now, SIGKILL in {} s if it's still running",
            crate::jobs::STOP_GRACE.as_secs()
        ),
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
    Ok(())
}
