//! Jobs in the `jobs` table and their log files.
//!
//! States: running → finished (exit 0) / crashed (≠ 0) / stopped (stop request) / lost
//! (the wrapper vanished without reporting, or the agent restarted while it ran).

use std::path::{Path, PathBuf};

use anyhow::Result;
use pocketgpu_proto::local::JobStart;
use pocketgpu_proto::msg::jobs::{JobDetail, JobState, JobSummary};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::store::db::now_ms;

/// Per job; when reached, the end of the output is kept (F07).
pub const LOG_CAP_BYTES: u64 = 50 * 1024 * 1024;
/// All job logs together; the oldest finished logs go first.
pub const TOTAL_LOG_CAP_BYTES: u64 = 1024 * 1024 * 1024;
pub const KEEP_DAYS: i64 = 30;
/// Lines of output in `GetJob`.
pub const TAIL_LINES: usize = 20;

pub fn state_str(s: JobState) -> &'static str {
    match s {
        JobState::Running => "running",
        JobState::Finished => "finished",
        JobState::Crashed => "crashed",
        JobState::Stopped => "stopped",
        JobState::Lost | JobState::Unknown => "lost",
    }
}

fn parse_state(s: &str) -> JobState {
    match s {
        "running" => JobState::Running,
        "finished" => JobState::Finished,
        "crashed" => JobState::Crashed,
        "stopped" => JobState::Stopped,
        "lost" => JobState::Lost,
        _ => JobState::Unknown,
    }
}

/// How a job ended, from the wrapper's report.
pub fn end_state(exit_code: Option<i32>, stopped: bool) -> JobState {
    match (stopped, exit_code) {
        (true, _) => JobState::Stopped,
        (false, Some(0)) => JobState::Finished,
        _ => JobState::Crashed,
    }
}

/// Records a new running job; returns its ID and log file.
pub fn create(db: &Connection, start: &JobStart, logs_dir: &Path) -> Result<(u64, PathBuf)> {
    let name: String = start.name.chars().take(80).collect();
    db.execute(
        "INSERT INTO jobs (name, command, cwd, started_at, state, log_path, alerts_on)
         VALUES (?1, ?2, ?3, ?4, 'running', '', ?5)",
        params![name, start.command, start.cwd, now_ms(), start.alerts],
    )?;
    let id = db.last_insert_rowid() as u64;
    let log = logs_dir.join(format!("{id}.log"));
    db.execute(
        "UPDATE jobs SET log_path = ?1 WHERE job_id = ?2",
        params![log.to_string_lossy(), id as i64],
    )?;
    Ok((id, log))
}

/// Ends a running job and returns its new state. Ended jobs are left as they are.
pub fn end(db: &Connection, id: u64, exit_code: Option<i32>, stopped: bool) -> Result<JobState> {
    let state = end_state(exit_code, stopped);
    let log_bytes = log_path(db, id)?
        .and_then(|p| std::fs::metadata(p).ok())
        .map_or(0, |m| m.len());
    db.execute(
        "UPDATE jobs SET state = ?1, exit_code = ?2, ended_at = ?3, log_bytes = ?4
         WHERE job_id = ?5 AND state = 'running'",
        params![
            state_str(state),
            exit_code,
            now_ms(),
            log_bytes as i64,
            id as i64
        ],
    )?;
    Ok(state)
}

pub fn mark_lost(db: &Connection, id: u64) -> Result<()> {
    db.execute(
        "UPDATE jobs SET state = 'lost', ended_at = ?1 WHERE job_id = ?2 AND state = 'running'",
        params![now_ms(), id as i64],
    )?;
    Ok(())
}

/// At agent start: jobs still marked running lost their wrapper with the old agent.
pub fn lose_running(db: &Connection) -> Result<usize> {
    Ok(db.execute(
        "UPDATE jobs SET state = 'lost', ended_at = ?1 WHERE state = 'running'",
        [now_ms()],
    )?)
}

pub fn set_alerts(db: &Connection, id: u64, on: bool) -> Result<bool> {
    Ok(db.execute(
        "UPDATE jobs SET alerts_on = ?1 WHERE job_id = ?2",
        params![on, id as i64],
    )? == 1)
}

pub fn bump_peak_vram(db: &Connection, id: u64, mb: u64) -> Result<()> {
    db.execute(
        "UPDATE jobs SET peak_vram_mb = max(coalesce(peak_vram_mb, 0), ?1) WHERE job_id = ?2",
        params![mb as i64, id as i64],
    )?;
    Ok(())
}

fn summary(r: &Row<'_>) -> rusqlite::Result<JobSummary> {
    Ok(JobSummary {
        id: r.get::<_, i64>("job_id")? as u64,
        name: r.get("name")?,
        state: parse_state(&r.get::<_, String>("state")?),
        started_ms: r.get::<_, i64>("started_at")? as u64,
        ended_ms: r.get::<_, Option<i64>>("ended_at")?.map(|v| v as u64),
        exit_code: r.get("exit_code")?,
    })
}

/// Newest first; `before` pages back by job ID.
pub fn list(db: &Connection, limit: u32, before: Option<u64>) -> Result<Vec<JobSummary>> {
    let mut stmt =
        db.prepare("SELECT * FROM jobs WHERE job_id < ?1 ORDER BY job_id DESC LIMIT ?2")?;
    let before = before.map_or(i64::MAX, |b| b as i64);
    let rows = stmt.query_map(params![before, limit.min(200)], summary)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(db: &Connection, id: u64) -> Result<Option<(JobDetail, PathBuf)>> {
    let row = db
        .query_row("SELECT * FROM jobs WHERE job_id = ?1", [id as i64], |r| {
            Ok((
                summary(r)?,
                r.get::<_, String>("command")?,
                r.get::<_, Option<i64>>("peak_vram_mb")?,
                r.get::<_, i64>("log_bytes")?,
                r.get::<_, bool>("alerts_on")?,
                r.get::<_, String>("log_path")?,
            ))
        })
        .optional()?;
    Ok(row.map(|(summary, command, peak, bytes, alerts_on, log)| {
        let log = PathBuf::from(log);
        let log_bytes = std::fs::metadata(&log).map_or(bytes as u64, |m| m.len());
        let detail = JobDetail {
            summary,
            command,
            peak_vram_mb: peak.map(|v| v as u64),
            log_bytes,
            alerts_on,
            tail: tail(&log, TAIL_LINES),
        };
        (detail, log)
    }))
}

fn log_path(db: &Connection, id: u64) -> Result<Option<PathBuf>> {
    Ok(db
        .query_row(
            "SELECT log_path FROM jobs WHERE job_id = ?1",
            [id as i64],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(PathBuf::from))
}

/// The last `n` lines of a log as a person would see them: progress bars redraw with `\r`,
/// so only the text after the last `\r` of each line counts.
pub fn tail(path: &Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    const WINDOW: u64 = 64 * 1024;
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(WINDOW)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text
        .split('\n')
        // Terminals end lines with \r\n; that \r isn't a redraw.
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .map(|l| l.rsplit('\r').next().unwrap_or(l).to_string())
        .collect();
    if len > WINDOW && !lines.is_empty() {
        lines.remove(0); // probably cut in the middle
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(n);
    lines.split_off(start)
}

/// Deletes logs of jobs that ended over 30 days ago, then the oldest ended logs until all
/// logs fit in [`TOTAL_LOG_CAP_BYTES`]. Job rows older than 30 days go too.
pub fn prune(db: &Connection) -> Result<()> {
    let cutoff = now_ms() - KEEP_DAYS * 24 * 3600 * 1000;
    let mut stmt = db.prepare(
        "SELECT job_id, log_path, coalesce(ended_at, 0) FROM jobs
         WHERE state != 'running' ORDER BY coalesce(ended_at, 0) ASC",
    )?;
    let ended: Vec<(i64, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let size = |p: &str| std::fs::metadata(p).map_or(0, |m| m.len());
    let mut total: u64 = {
        let mut all = db.prepare("SELECT log_path FROM jobs")?;
        let paths: Vec<String> = all
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        paths.iter().map(|p| size(p)).sum()
    };
    for (id, path, ended_at) in ended {
        if ended_at < cutoff {
            total = total.saturating_sub(size(&path));
            let _ = std::fs::remove_file(&path);
            db.execute("DELETE FROM jobs WHERE job_id = ?1", [id])?;
        } else if total > TOTAL_LOG_CAP_BYTES {
            total = total.saturating_sub(size(&path));
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::open_in_memory;

    fn start(name: &str) -> JobStart {
        JobStart {
            name: name.into(),
            command: "python train.py".into(),
            cwd: "/home/ana".into(),
            alerts: true,
        }
    }

    #[test]
    fn jobs_move_through_their_states() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_in_memory().unwrap();
        let (a, log) = create(&db, &start("train.py"), dir.path()).unwrap();
        assert!(log.ends_with(format!("{a}.log")));
        let (b, _) = create(&db, &start("eval.py"), dir.path()).unwrap();
        let (c, _) = create(&db, &start("other"), dir.path()).unwrap();
        assert_eq!(end(&db, a, Some(0), false).unwrap(), JobState::Finished);
        assert_eq!(end(&db, b, Some(1), false).unwrap(), JobState::Crashed);
        assert_eq!(lose_running(&db).unwrap(), 1);
        // An ended job doesn't change again.
        end(&db, a, Some(9), false).unwrap();
        let jobs = list(&db, 10, None).unwrap();
        assert_eq!(jobs.iter().map(|j| j.id).collect::<Vec<_>>(), vec![c, b, a]);
        assert_eq!(jobs[0].state, JobState::Lost);
        assert_eq!(jobs[2].exit_code, Some(0));
        assert_eq!(list(&db, 10, Some(b)).unwrap().len(), 1);
    }

    #[test]
    fn stopped_wins_over_exit_code() {
        assert_eq!(end_state(Some(143), true), JobState::Stopped);
        assert_eq!(end_state(None, false), JobState::Crashed);
    }

    #[test]
    fn detail_has_the_last_lines_as_seen() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_in_memory().unwrap();
        let (id, log) = create(&db, &start("train.py"), dir.path()).unwrap();
        let mut out = String::new();
        for i in 0..30 {
            out += &format!("epoch {i}\n");
        }
        out += "50%|█████     |\r100%|██████████|\r\n";
        std::fs::write(&log, out).unwrap();
        bump_peak_vram(&db, id, 9000).unwrap();
        bump_peak_vram(&db, id, 4000).unwrap();
        let (detail, _) = get(&db, id).unwrap().unwrap();
        assert_eq!(detail.tail.len(), TAIL_LINES);
        assert_eq!(detail.tail.last().unwrap(), "100%|██████████|");
        assert_eq!(detail.peak_vram_mb, Some(9000));
        assert!(detail.alerts_on);
        assert!(set_alerts(&db, id, false).unwrap());
    }

    #[test]
    fn prune_removes_old_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_in_memory().unwrap();
        let (id, log) = create(&db, &start("old"), dir.path()).unwrap();
        std::fs::write(&log, "x").unwrap();
        end(&db, id, Some(0), false).unwrap();
        db.execute("UPDATE jobs SET ended_at = 0", []).unwrap();
        prune(&db).unwrap();
        assert!(!log.exists());
        assert!(get(&db, id).unwrap().is_none());
    }
}
