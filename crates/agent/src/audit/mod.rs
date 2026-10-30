//! Audit log: every state-changing or refused request, kept 90 days.

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::store::db::now_ms;

pub const KEEP_DAYS: i64 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Pair,
    Revoke,
    PermissionChange,
    ActionRun,
    ActionCreate,
    ActionEdit,
    ActionDelete,
    Kill,
    JobStop,
    AlertSettings,
    Refused,
    ShellOpen,
    ShellClose,
    /// `pgpu shell on` or `off`.
    ShellAccess,
    ModelPull,
    ModelDelete,
    RelayChange,
    /// A phone started, stopped or restarted a container (F28).
    Container,
    /// `pgpu docker on` or `off`.
    DockerAccess,
}

impl Event {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pair => "pair",
            Self::Revoke => "revoke",
            Self::PermissionChange => "permission_change",
            Self::ActionRun => "action_run",
            Self::ActionCreate => "action_create",
            Self::ActionEdit => "action_edit",
            Self::ActionDelete => "action_delete",
            Self::Kill => "kill",
            Self::JobStop => "job_stop",
            Self::AlertSettings => "alert_settings",
            Self::Refused => "refused",
            Self::ShellOpen => "shell_open",
            Self::ShellClose => "shell_close",
            Self::ShellAccess => "shell_access",
            Self::ModelPull => "model_pull",
            Self::ModelDelete => "model_delete",
            Self::RelayChange => "relay_change",
            Self::Container => "container",
            Self::DockerAccess => "docker_access",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub at_ms: i64,
    pub device: String,
    pub event: String,
    pub target: String,
    pub result: String,
}

/// `device` is the phone's key, or `local` for the command line.
pub fn record(
    db: &Connection,
    device: &str,
    event: Event,
    target: &str,
    result: &str,
) -> Result<()> {
    db.execute(
        "INSERT INTO audit (at, device_id, event, target, result) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            now_ms(),
            device,
            event.as_str(),
            clean(target),
            clean(result)
        ],
    )?;
    Ok(())
}

/// Longest target or result kept: phones choose much of this text (refused requests).
const MAX_TEXT: usize = 200;

/// Without control characters (which could redraw `pgpu audit` in a terminal) and cut to
/// [`MAX_TEXT`] characters, so a phone can't fill the disk one entry at a time.
pub fn clean(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TEXT)
        .collect();
    if text.chars().count() > MAX_TEXT {
        out.push('…');
    }
    out
}

/// Entries returned at most: the reply to `pgpu audit` is one local frame.
const MAX_ENTRIES: u32 = 2000;

/// The newest [`MAX_ENTRIES`] entries at or after `since_ms`, newest first.
pub fn since(db: &Connection, since_ms: i64) -> Result<Vec<Entry>> {
    let mut stmt = db.prepare(&format!(
        "SELECT at, device_id, event, target, result FROM audit WHERE at >= ?1 \
         ORDER BY at DESC LIMIT {MAX_ENTRIES}"
    ))?;
    let rows = stmt.query_map([since_ms], |r| {
        Ok(Entry {
            at_ms: r.get(0)?,
            device: r.get(1)?,
            event: r.get(2)?,
            target: r.get(3)?,
            result: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Entries with phone names instead of keys, for `pgpu audit`. Phones that were removed
/// since show the start of their key.
pub fn named(db: &Connection, since_ms: i64) -> Result<Vec<pocketgpu_proto::local::AuditEntry>> {
    let mut stmt = db.prepare(
        "SELECT a.at, a.device_id, d.name, a.event, a.target, a.result
         FROM audit a LEFT JOIN devices d ON d.device_id = a.device_id
         WHERE a.at >= ?1 ORDER BY a.at DESC, a.audit_id DESC",
    )?;
    let rows = stmt.query_map([since_ms], |r| {
        let key: String = r.get(1)?;
        let name: Option<String> = r.get(2)?;
        Ok(pocketgpu_proto::local::AuditEntry {
            at_ms: r.get(0)?,
            device: name.unwrap_or_else(|| {
                if key == "local" {
                    key.clone()
                } else {
                    format!("{}…", key.chars().take(8).collect::<String>())
                }
            }),
            event: r.get(3)?,
            target: r.get(4)?,
            result: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn prune(db: &Connection) -> Result<usize> {
    let cutoff = now_ms() - KEEP_DAYS * 24 * 3600 * 1000;
    Ok(db.execute("DELETE FROM audit WHERE at < ?1", [cutoff])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_text_is_cut_and_cannot_redraw_the_terminal() {
        assert_eq!(clean("pid 9"), "pid 9");
        assert_eq!(clean("a\x1b[2Kb\nc"), "a [2Kb c");
        let long = clean(&"x".repeat(1_000_000));
        assert_eq!(long.chars().count(), MAX_TEXT + 1);
    }

    #[test]
    fn records_and_reads_back() {
        let db = crate::store::db::open_in_memory().unwrap();
        record(
            &db,
            "abcd",
            Event::Refused,
            "KillProcess 42",
            "needs control permission",
        )
        .unwrap();
        let entries = since(&db, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event, "refused");
    }
}
