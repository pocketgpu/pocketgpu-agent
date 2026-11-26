//! Shares, their guest and their daily counts in the agent's database (F48). Counts only:
//! nothing a guest asks or a model answers is ever written here.

use anyhow::Result;
use pocketgpu_proto::share::{ModelServerRef, NewShare, ShareState};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub id: u64,
    pub server: ModelServerRef,
    pub model: String,
    pub label: String,
    pub from_name: Option<String>,
    pub secret: [u8; 32],
    pub daily_limit: Option<u32>,
    pub pause_for_jobs: bool,
    pub ends_ms: Option<i64>,
    pub claim_until_ms: i64,
    pub state: ShareState,
    pub ended_ms: Option<i64>,
    pub made_by: String,
    pub created_ms: i64,
}

fn server_text(s: ModelServerRef) -> String {
    match s {
        ModelServerRef::Ollama => "ollama".into(),
        ModelServerRef::Port(p) => p.to_string(),
    }
}

fn server_from(text: &str) -> ModelServerRef {
    text.parse()
        .map_or(ModelServerRef::Ollama, ModelServerRef::Port)
}

fn state_text(s: ShareState) -> &'static str {
    match s {
        ShareState::Waiting => "waiting",
        ShareState::Active => "active",
        ShareState::Ended => "ended",
    }
}

fn state_from(text: &str) -> ShareState {
    match text {
        "waiting" => ShareState::Waiting,
        "active" => ShareState::Active,
        _ => ShareState::Ended,
    }
}

const COLUMNS: &str = "id, server, model, label, from_name, secret, daily_limit, pause_for_jobs, \
     ends_ms, claim_until_ms, state, ended_ms, made_by, created_ms";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Share> {
    let secret: Vec<u8> = r.get(5)?;
    Ok(Share {
        id: r.get::<_, i64>(0)? as u64,
        server: server_from(&r.get::<_, String>(1)?),
        model: r.get(2)?,
        label: r.get(3)?,
        from_name: r.get(4)?,
        secret: secret.try_into().unwrap_or([0; 32]),
        daily_limit: r.get::<_, Option<i64>>(6)?.map(|l| l as u32),
        pause_for_jobs: r.get::<_, i64>(7)? != 0,
        ends_ms: r.get(8)?,
        claim_until_ms: r.get(9)?,
        state: state_from(&r.get::<_, String>(10)?),
        ended_ms: r.get(11)?,
        made_by: r.get(12)?,
        created_ms: r.get(13)?,
    })
}

pub fn insert(
    db: &Connection,
    new: &NewShare,
    secret: [u8; 32],
    made_by: &str,
    now_ms: i64,
) -> Result<Share> {
    let ends_ms = new.minutes.map(|m| now_ms + i64::from(m) * 60_000);
    let claim_until_ms = ends_ms.map_or(now_ms + pocketgpu_proto::share::CLAIM_WITHIN_MS, |e| {
        e.min(now_ms + pocketgpu_proto::share::CLAIM_WITHIN_MS)
    });
    let from = new.from.as_deref().map(str::trim).filter(|f| !f.is_empty());
    db.execute(
        "INSERT INTO model_shares (server, model, label, from_name, secret, daily_limit, \
         pause_for_jobs, ends_ms, claim_until_ms, state, made_by, created_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'waiting', ?10, ?11)",
        params![
            server_text(new.server),
            new.model.trim(),
            new.label.trim(),
            from,
            secret.to_vec(),
            new.daily_limit.map(i64::from),
            i64::from(new.pause_for_jobs),
            ends_ms,
            claim_until_ms,
            made_by,
            now_ms,
        ],
    )?;
    let id = db.last_insert_rowid() as u64;
    get(db, id)?.ok_or_else(|| anyhow::anyhow!("the share was not saved"))
}

pub fn get(db: &Connection, id: u64) -> Result<Option<Share>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM model_shares WHERE id = ?1"),
            [id as i64],
            row,
        )
        .optional()?)
}

/// Every share not swept yet, newest first.
pub fn list(db: &Connection) -> Result<Vec<Share>> {
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNS} FROM model_shares ORDER BY created_ms DESC"
    ))?;
    let rows = q.query_map([], row)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn count_open(db: &Connection) -> Result<usize> {
    let n: i64 = db.query_row(
        "SELECT COUNT(*) FROM model_shares WHERE state != 'ended'",
        [],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

pub fn set_state(db: &Connection, id: u64, state: ShareState, now_ms: i64) -> Result<()> {
    let ended = (state == ShareState::Ended).then_some(now_ms);
    db.execute(
        "UPDATE model_shares SET state = ?2, ended_ms = COALESCE(ended_ms, ?3) WHERE id = ?1",
        params![id as i64, state_text(state), ended],
    )?;
    Ok(())
}

/// A share's new end, daily limit and pause. A link not opened yet can be opened until the
/// new end, at most a week after it was made.
pub fn change(
    db: &Connection,
    id: u64,
    ends_ms: Option<i64>,
    daily_limit: Option<u32>,
    pause_for_jobs: bool,
) -> Result<()> {
    db.execute(
        "UPDATE model_shares SET ends_ms = ?2, daily_limit = ?3, pause_for_jobs = ?4, \
         claim_until_ms = MIN(created_ms + ?5, COALESCE(?2, created_ms + ?5)) WHERE id = ?1",
        params![
            id as i64,
            ends_ms,
            daily_limit.map(i64::from),
            i64::from(pause_for_jobs),
            pocketgpu_proto::share::CLAIM_WITHIN_MS,
        ],
    )?;
    Ok(())
}

/// The share this guest key belongs to.
pub fn share_of_guest(db: &Connection, key: &str) -> Result<Option<u64>> {
    Ok(db
        .query_row(
            "SELECT share_id FROM model_guests WHERE key = ?1",
            [key],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|id| id as u64))
}

pub fn guest_of_share(db: &Connection, share: u64) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT key FROM model_guests WHERE share_id = ?1",
            [share as i64],
            |r| r.get(0),
        )
        .optional()?)
}

/// Adds the guest; false when the share already has one or the key is a guest elsewhere.
/// Only through [`claim_guest`], which checks the share still waits.
fn insert_guest(db: &Connection, key: &str, share: u64, now_ms: i64) -> Result<bool> {
    let n = db.execute(
        "INSERT OR IGNORE INTO model_guests (key, share_id, claimed_ms, last_seen_ms) \
         VALUES (?1, ?2, ?3, ?3)",
        params![key, share as i64, now_ms],
    )?;
    Ok(n == 1)
}

/// How a claim came out, decided in one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The key is the share's guest now and the share is active.
    Made,
    /// The share has a guest already, or the key is a guest of another share.
    Taken,
    /// The share still waits, but its link can't be opened any more.
    Expired,
    /// The share has ended or is gone.
    Ended,
}

/// Makes `key` the share's only guest and the share active, only while it still waits and
/// its link still opens at `now`. The state is checked in the same transaction as the
/// change, so a Stop or the sweeper that ended the share since it was read stays ended.
pub fn claim_guest(db: &Connection, key: &str, share: u64, now_ms: i64) -> Result<Claim> {
    let tx = db.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE model_shares SET state = 'active' WHERE id = ?1 AND state = 'waiting' \
         AND claim_until_ms > ?2 AND (ends_ms IS NULL OR ends_ms > ?2)",
        params![share as i64, now_ms],
    )?;
    if n == 0 {
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM model_shares WHERE id = ?1",
                [share as i64],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(match state.as_deref() {
            Some("active") => Claim::Taken,
            Some("waiting") => Claim::Expired,
            _ => Claim::Ended,
        });
    }
    if !insert_guest(&tx, key, share, now_ms)? {
        // Dropping the transaction puts the share back to waiting.
        return Ok(Claim::Taken);
    }
    tx.commit()?;
    Ok(Claim::Made)
}

pub fn remove_guest(db: &Connection, share: u64) -> Result<()> {
    db.execute(
        "DELETE FROM model_guests WHERE share_id = ?1",
        [share as i64],
    )?;
    Ok(())
}

/// Written at most once a minute per guest.
pub fn touch_guest(db: &Connection, key: &str, now_ms: i64) -> Result<()> {
    db.execute(
        "UPDATE model_guests SET last_seen_ms = ?2 WHERE key = ?1 AND last_seen_ms < ?2 - 60000",
        params![key, now_ms],
    )?;
    Ok(())
}

pub fn used_on(db: &Connection, share: u64, day: &str) -> Result<u32> {
    Ok(db
        .query_row(
            "SELECT messages FROM model_usage WHERE share_id = ?1 AND day = ?2",
            params![share as i64, day],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0) as u32)
}

/// Counts one message; returns the day's count after it.
pub fn count_message(db: &Connection, share: u64, day: &str) -> Result<u32> {
    db.execute(
        "INSERT INTO model_usage (share_id, day, messages) VALUES (?1, ?2, 1) \
         ON CONFLICT (share_id, day) DO UPDATE SET messages = messages + 1",
        params![share as i64, day],
    )?;
    used_on(db, share, day)
}

/// Shares whose time is up and not ended yet: waiting past the claim time, or past the end.
pub fn due(db: &Connection, now_ms: i64) -> Result<Vec<u64>> {
    let mut q = db.prepare(
        "SELECT id FROM model_shares WHERE state != 'ended' AND \
         ((state = 'waiting' AND claim_until_ms <= ?1) OR (ends_ms IS NOT NULL AND ends_ms <= ?1))",
    )?;
    let ids = q
        .query_map([now_ms], |r| r.get::<_, i64>(0))?
        .map(|r| r.map(|id| id as u64))
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Forgets shares ended a week ago and counts older than 31 days.
pub fn purge(db: &Connection, now_ms: i64, today_minus_31: &str) -> Result<()> {
    db.execute(
        "DELETE FROM model_shares WHERE state = 'ended' AND ended_ms <= ?1",
        [now_ms - 7 * 86_400_000],
    )?;
    db.execute("DELETE FROM model_usage WHERE day < ?1", [today_minus_31])?;
    Ok(())
}

/// The soonest time a share's time is up, for the sweeper.
pub fn soonest_due(db: &Connection) -> Result<Option<i64>> {
    Ok(db.query_row(
        "SELECT MIN(CASE WHEN state = 'waiting' THEN MIN(claim_until_ms, COALESCE(ends_ms, claim_until_ms)) \
         ELSE ends_ms END) FROM model_shares WHERE state != 'ended'",
        [],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn waiting(db: &Connection) -> Share {
        let new = NewShare {
            server: ModelServerRef::Ollama,
            model: "llama3:8b".into(),
            label: "Mum".into(),
            from: None,
            minutes: None,
            daily_limit: None,
            pause_for_jobs: true,
        };
        insert(db, &new, [7; 32], "owner", NOW).unwrap()
    }

    fn state(db: &Connection, id: u64) -> ShareState {
        get(db, id).unwrap().unwrap().state
    }

    #[test]
    fn a_claim_after_a_stop_leaves_the_share_ended() {
        let db = crate::store::db::open_in_memory().unwrap();
        let s = waiting(&db);
        // The claim read the share while it waited; then Stop or the sweeper ended it.
        remove_guest(&db, s.id).unwrap();
        set_state(&db, s.id, ShareState::Ended, NOW).unwrap();
        assert_eq!(claim_guest(&db, "guest", s.id, NOW).unwrap(), Claim::Ended);
        assert_eq!(state(&db, s.id), ShareState::Ended);
        assert_eq!(guest_of_share(&db, s.id).unwrap(), None);
    }

    #[test]
    fn a_waiting_share_is_claimed_once() {
        let db = crate::store::db::open_in_memory().unwrap();
        let s = waiting(&db);
        assert_eq!(claim_guest(&db, "first", s.id, NOW).unwrap(), Claim::Made);
        assert_eq!(state(&db, s.id), ShareState::Active);
        assert_eq!(claim_guest(&db, "second", s.id, NOW).unwrap(), Claim::Taken);
        assert_eq!(guest_of_share(&db, s.id).unwrap().as_deref(), Some("first"));
    }

    #[test]
    fn a_key_already_a_guest_leaves_the_other_share_waiting() {
        let db = crate::store::db::open_in_memory().unwrap();
        let (a, b) = (waiting(&db), waiting(&db));
        assert_eq!(claim_guest(&db, "guest", a.id, NOW).unwrap(), Claim::Made);
        assert_eq!(claim_guest(&db, "guest", b.id, NOW).unwrap(), Claim::Taken);
        assert_eq!(state(&db, b.id), ShareState::Waiting);
        assert_eq!(guest_of_share(&db, b.id).unwrap(), None);
    }

    #[test]
    fn a_link_past_its_time_is_not_claimed() {
        let db = crate::store::db::open_in_memory().unwrap();
        let s = waiting(&db);
        let late = s.claim_until_ms;
        assert_eq!(
            claim_guest(&db, "guest", s.id, late).unwrap(),
            Claim::Expired
        );
        assert_eq!(state(&db, s.id), ShareState::Waiting);
        assert_eq!(
            claim_guest(&db, "guest", s.id + 1, NOW).unwrap(),
            Claim::Ended
        );
    }
}
