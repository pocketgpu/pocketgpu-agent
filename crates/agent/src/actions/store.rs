//! Actions and their runs in SQLite (`actions`, `action_runs`).

use anyhow::Result;
use pocketgpu_proto::msg::control::{Action, ActionRun};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Invalid, Valid};

/// Runs are kept this long (data-model: 30 days).
pub const KEEP_RUNS_DAYS: i64 = 30;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Invalid(#[from] Invalid),
    #[error("no action with id {0}")]
    NotFound(u64),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Action> {
    Ok(Action {
        id: r.get::<_, i64>(0)? as u64,
        name: r.get(1)?,
        command: r.get(2)?,
        icon: r.get(3)?,
        risky: r.get(4)?,
        timeout_s: r.get(5)?,
    })
}

const COLUMNS: &str = "action_id, name, command, icon, risky, timeout_s";

pub fn list(db: &Connection) -> Result<Vec<Action>> {
    let mut stmt = db.prepare(&format!("SELECT {COLUMNS} FROM actions ORDER BY action_id"))?;
    let rows = stmt.query_map([], row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn get(db: &Connection, id: u64) -> Result<Option<Action>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM actions WHERE action_id = ?1"),
            [id as i64],
            row,
        )
        .optional()?)
}

/// Finds an action by its id or its name (for the command line).
pub fn find(db: &Connection, id_or_name: &str) -> Result<Option<Action>> {
    if let Ok(id) = id_or_name.parse::<u64>()
        && let Some(a) = get(db, id)?
    {
        return Ok(Some(a));
    }
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM actions WHERE name = ?1 COLLATE NOCASE"),
            [id_or_name],
            row,
        )
        .optional()?)
}

pub fn create(db: &Connection, a: &Valid) -> Result<Action, StoreError> {
    let res = db.execute(
        "INSERT INTO actions (name, command, icon, risky, timeout_s) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![a.name, a.command, a.icon, a.risky, a.timeout_s],
    );
    check_duplicate(res, &a.name)?;
    let id = db.last_insert_rowid() as u64;
    get(db, id).ok().flatten().ok_or(StoreError::NotFound(id))
}

pub fn edit(db: &Connection, id: u64, a: &Valid) -> Result<Action, StoreError> {
    let res = db.execute(
        "UPDATE actions SET name = ?1, command = ?2, icon = ?3, risky = ?4, timeout_s = ?5
         WHERE action_id = ?6",
        params![a.name, a.command, a.icon, a.risky, a.timeout_s, id as i64],
    );
    if check_duplicate(res, &a.name)? == 0 {
        return Err(StoreError::NotFound(id));
    }
    get(db, id).ok().flatten().ok_or(StoreError::NotFound(id))
}

pub fn delete(db: &Connection, id: u64) -> Result<(), StoreError> {
    if db.execute("DELETE FROM actions WHERE action_id = ?1", [id as i64])? == 0 {
        return Err(StoreError::NotFound(id));
    }
    db.execute("DELETE FROM action_runs WHERE action_id = ?1", [id as i64])?;
    Ok(())
}

/// Names are unique per machine; SQLite reports a clash as a constraint error.
fn check_duplicate(res: rusqlite::Result<usize>, name: &str) -> Result<usize, StoreError> {
    match res {
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Err(Invalid::Duplicate(name.to_string()).into())
        }
        other => Ok(other?),
    }
}

pub fn start_run(db: &Connection, action_id: u64, device: Option<&str>, at_ms: i64) -> Result<u64> {
    db.execute(
        "INSERT INTO action_runs (action_id, device_id, started_at) VALUES (?1, ?2, ?3)",
        params![action_id as i64, device, at_ms],
    )?;
    Ok(db.last_insert_rowid() as u64)
}

pub fn end_run(
    db: &Connection,
    run_id: u64,
    at_ms: i64,
    exit_code: Option<i32>,
    output: &str,
) -> Result<()> {
    db.execute(
        "UPDATE action_runs SET ended_at = ?1, exit_code = ?2, output = ?3 WHERE run_id = ?4",
        params![at_ms, exit_code, output, run_id as i64],
    )?;
    Ok(())
}

/// Recent runs, newest first, with the action's and phone's names.
pub fn runs(db: &Connection, limit: u32) -> Result<Vec<ActionRun>> {
    let mut stmt = db.prepare(
        "SELECT r.run_id, r.action_id, a.name, d.name, r.started_at, r.ended_at, r.exit_code,
                a.timeout_s
         FROM action_runs r
         JOIN actions a ON a.action_id = r.action_id
         LEFT JOIN devices d ON d.device_id = r.device_id
         ORDER BY r.started_at DESC, r.run_id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit.min(200)], |r| {
        let started: i64 = r.get(4)?;
        let ended: Option<i64> = r.get(5)?;
        let exit_code: Option<i32> = r.get(6)?;
        let timeout_s: i64 = r.get(7)?;
        Ok(ActionRun {
            id: r.get::<_, i64>(0)? as u64,
            action_id: r.get::<_, i64>(1)? as u64,
            action_name: r.get(2)?,
            device_name: r.get(3)?,
            started_ms: started as u64,
            ended_ms: ended.map(|e| e as u64),
            exit_code,
            timed_out: ended
                .is_some_and(|e| exit_code.is_none() && e - started >= timeout_s * 1000),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn prune(db: &Connection, now_ms: i64) -> Result<usize> {
    let cutoff = now_ms - KEEP_RUNS_DAYS * 24 * 3600 * 1000;
    Ok(db.execute("DELETE FROM action_runs WHERE started_at < ?1", [cutoff])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::validate;
    use pocketgpu_proto::msg::control::NewAction;

    fn valid(name: &str) -> Valid {
        validate(&NewAction {
            name: name.into(),
            command: "echo hi".into(),
            icon: "zap".into(),
            risky: true,
            timeout_s: Some(30),
        })
        .unwrap()
    }

    #[test]
    fn create_edit_delete() {
        let db = crate::store::db::open_in_memory().unwrap();
        let a = create(&db, &valid("Clear cache")).unwrap();
        assert!(a.risky);
        assert_eq!(a.timeout_s, 30);
        assert!(matches!(
            create(&db, &valid("Clear cache")),
            Err(StoreError::Invalid(Invalid::Duplicate(_)))
        ));
        let b = edit(&db, a.id, &valid("Clear the cache")).unwrap();
        assert_eq!(b.name, "Clear the cache");
        assert_eq!(find(&db, "clear the cache").unwrap().unwrap().id, a.id);
        assert_eq!(find(&db, &a.id.to_string()).unwrap().unwrap().id, a.id);
        delete(&db, a.id).unwrap();
        assert!(matches!(delete(&db, a.id), Err(StoreError::NotFound(_))));
        assert!(list(&db).unwrap().is_empty());
    }

    #[test]
    fn runs_are_listed_newest_first_and_pruned() {
        let db = crate::store::db::open_in_memory().unwrap();
        let a = create(&db, &valid("Status")).unwrap();
        let old = start_run(&db, a.id, None, 1_000).unwrap();
        end_run(&db, old, 2_000, Some(0), "ok").unwrap();
        let new = start_run(&db, a.id, Some("phone"), 10_000).unwrap();
        let listed = runs(&db, 10).unwrap();
        assert_eq!(listed[0].id, new);
        assert_eq!(listed[0].ended_ms, None);
        assert_eq!(listed[1].exit_code, Some(0));
        let day = 24 * 3600 * 1000;
        assert_eq!(prune(&db, 5_000 + KEEP_RUNS_DAYS * day).unwrap(), 1);
    }
}
