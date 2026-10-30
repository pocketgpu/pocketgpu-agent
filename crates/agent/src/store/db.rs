//! The agent's SQLite database. Tables follow `specs/001-v1-launch/data-model.md`.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

const MIGRATIONS: &[M<'static>] = &[M::up(
    "
    CREATE TABLE devices (
        device_id   TEXT PRIMARY KEY,
        name        TEXT NOT NULL CHECK (length(name) <= 40),
        platform    TEXT NOT NULL,
        permission  TEXT NOT NULL CHECK (permission IN ('view', 'control', 'shell')),
        push_ticket TEXT,
        paired_at   INTEGER NOT NULL,
        last_seen   INTEGER
    );
    CREATE TABLE actions (
        action_id INTEGER PRIMARY KEY,
        name      TEXT NOT NULL UNIQUE CHECK (length(name) <= 40),
        command   TEXT NOT NULL,
        icon      TEXT NOT NULL,
        risky     INTEGER NOT NULL DEFAULT 0,
        timeout_s INTEGER NOT NULL DEFAULT 600
    );
    CREATE TABLE action_runs (
        run_id     INTEGER PRIMARY KEY,
        action_id  INTEGER NOT NULL,
        device_id  TEXT,
        started_at INTEGER NOT NULL,
        ended_at   INTEGER,
        exit_code  INTEGER,
        output     TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE jobs (
        job_id       INTEGER PRIMARY KEY,
        name         TEXT NOT NULL,
        command      TEXT NOT NULL,
        cwd          TEXT NOT NULL,
        started_at   INTEGER NOT NULL,
        ended_at     INTEGER,
        exit_code    INTEGER,
        state        TEXT NOT NULL,
        peak_vram_mb INTEGER,
        log_path     TEXT NOT NULL,
        log_bytes    INTEGER NOT NULL DEFAULT 0,
        alerts_on    INTEGER NOT NULL DEFAULT 1
    );
    CREATE TABLE metrics_1m (
        at       INTEGER NOT NULL,
        gpu      INTEGER NOT NULL,
        util_min INTEGER, util_avg INTEGER, util_max INTEGER,
        vram_max_mb INTEGER, temp_max_c INTEGER,
        PRIMARY KEY (at, gpu)
    );
    CREATE TABLE audit (
        audit_id  INTEGER PRIMARY KEY,
        at        INTEGER NOT NULL,
        device_id TEXT NOT NULL,
        event     TEXT NOT NULL,
        target    TEXT NOT NULL DEFAULT '',
        result    TEXT NOT NULL
    );
    CREATE INDEX audit_at ON audit (at);
    CREATE TABLE alert_state (
        rule          TEXT NOT NULL,
        target        TEXT NOT NULL DEFAULT '',
        last_fired_at INTEGER NOT NULL,
        PRIMARY KEY (rule, target)
    );
    ",
)];

pub fn open(path: &Path) -> Result<Connection> {
    let mut conn =
        Connection::open(path).with_context(|| format!("could not open {}", path.display()))?;
    super::paths::set_private(path)?;
    prepare(&mut conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    prepare(&mut conn)?;
    Ok(conn)
}

fn prepare(conn: &mut Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Migrations::from_slice(MIGRATIONS)
        .to_latest(conn)
        .context("database migration failed")?;
    Ok(())
}

/// Current Unix time in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrations_are_valid() {
        super::Migrations::from_slice(super::MIGRATIONS)
            .validate()
            .unwrap();
        super::open_in_memory().unwrap();
    }
}
