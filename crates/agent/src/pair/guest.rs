//! Guests (F44, board 57): devices that joined with an invite, whose access ends by itself.
//! Making invites, joining with one, the guests list, an hour more, and the sweeper that
//! removes a guest when its time is up (audited, so the computer shows a notice).

use std::sync::Arc;
use std::time::Duration;

use iroh::PublicKey;
use pocketgpu_proto::Permission;
use pocketgpu_proto::msg::control::Guest;
use pocketgpu_proto::msg::pair::PairRequest;
use pocketgpu_proto::pairing::QrPayload;

use super::invite::{Grant, MAX_LEVEL, MAX_MINUTES, for_words, new_code};
use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;
use crate::store::devices;

/// The audit result when a guest's time ran out; shows the "access ended" notice.
pub const GUEST_ENDED: &str = "guest ended";
/// The longest the sweeper sleeps; it otherwise wakes when the next guest's time is up.
const SWEEP_EVERY: Duration = Duration::from_secs(15);
/// The shortest, so a guest that can't be removed doesn't keep it spinning.
const SWEEP_AT_LEAST: Duration = Duration::from_millis(50);
/// Wakes the sweeper when a guest joins, so it can wait for that guest's end.
static JOINED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// A new invite from `by` (a device with Shell), for at most [`MAX_LEVEL`]: the
/// `pocketgpu://pair?…` link and when the access would end. The words in the link (who,
/// what, until) are for the joining phone's screen only; the computer keeps the real ones.
pub async fn invite(
    agent: &Arc<Agent>,
    by: &PublicKey,
    permission: Permission,
    minutes: u32,
) -> Result<(String, i64), String> {
    if minutes == 0 || minutes > MAX_MINUTES {
        return Err("a guest can have the computer from a minute up to a week".into());
    }
    if permission > MAX_LEVEL {
        return Err("a guest can only look or run actions; Full control can't be lent".into());
    }
    let by_name = devices::get(&agent.db(), &by.to_string())
        .ok()
        .flatten()
        .map_or_else(|| "Someone".to_string(), |d| d.name);
    let code = new_code().map_err(|e| e.to_string())?;
    // Argon2id: slow on purpose, so off the async threads.
    let secret = tokio::task::spawn_blocking(move || code.secret())
        .await
        .map_err(|e| e.to_string())?;
    let ends_ms = now_ms() + i64::from(minutes) * 60_000;
    let grant = Grant {
        permission,
        ends_ms,
        minutes,
        by: by_name.clone(),
        by_key: *by,
    };
    agent.pairing.invites.add(secret, grant)?;
    let relays = agent
        .config
        .read()
        .expect("config lock poisoned")
        .relays
        .clone();
    let qr = QrPayload {
        agent_id: agent.identity.agent_id().to_string(),
        code,
        relays,
    };
    let uri = format!(
        "{}&guest=1&name={}&by={}&perm={}&until={ends_ms}",
        qr.to_uri(),
        escape(&agent.name()),
        escape(&by_name),
        permission.as_str()
    );
    let _ = audit::record(
        &agent.db(),
        &by.to_string(),
        Event::Invite,
        permission.as_str(),
        &for_words(minutes),
    );
    Ok((uri, ends_ms))
}

fn escape(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Whether an invite may still be used: its level is one a guest may have, and the device
/// that made it is still paired with Shell and is no guest itself (revoking, unpairing or
/// lowering it also forgets its invites; this covers the rest).
pub fn may_join(agent: &Agent, grant: &Grant) -> Result<(), &'static str> {
    if grant.permission > MAX_LEVEL {
        return Err("a guest can only look or run actions");
    }
    let by = devices::get(&agent.db(), &grant.by_key.to_string())
        .ok()
        .flatten();
    let lends = agent.permission_of(&grant.by_key) >= Some(Permission::Shell)
        && by.is_some_and(|d| d.expires_ms.is_none());
    if !lends {
        return Err("the device that made this invite can no longer share this computer");
    }
    Ok(())
}

/// What [`join`] says if a device paired for good got that far: it keeps what it has.
pub const ALREADY_PAIRED: &str =
    "this device is already paired with this computer, so it doesn't need an invite";

/// The level of `device` if it is paired for good (no guest). An invite would only make it a
/// guest, maybe with less, that is removed when the time is up, so the handler lets it in
/// with this level instead (an app reinstalled with its key kept saves the computer again),
/// changing nothing and leaving the invite for whoever it was for.
pub fn paired_for_good(agent: &Agent, device: &PublicKey) -> Option<Permission> {
    devices::get(&agent.db(), &device.to_string())
        .ok()
        .flatten()
        .filter(|d| d.expires_ms.is_none())
        .and_then(|d| d.permission.parse().ok())
}

/// Pairs a device that proved an invite, with the invite's level and end time. Never over a
/// device paired for good (the handler answers that one itself, so its invite isn't used up).
pub fn join(
    agent: &Agent,
    device: PublicKey,
    req: &PairRequest,
    grant: &Grant,
) -> anyhow::Result<()> {
    let id = device.to_string();
    let name = super::clean_name(&req.device_name);
    let ticket = req
        .push_ticket
        .as_deref()
        .filter(|t| t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()));
    let db = agent.db();
    if devices::get(&db, &id)?.is_some_and(|d| d.expires_ms.is_none()) {
        anyhow::bail!(ALREADY_PAIRED);
    }
    devices::upsert(&db, &id, &name, req.platform, grant.permission, ticket)?;
    devices::set_expiry(&db, &id, Some(grant.ends_ms))?;
    audit::record(
        &db,
        &id,
        Event::Pair,
        &name,
        &format!(
            "{} · guest {} · from {}",
            grant.permission.as_str(),
            for_words(grant.minutes),
            grant.by
        ),
    )?;
    JOINED.notify_one();
    Ok(())
}

/// The guests now, soonest end first.
pub fn list(agent: &Agent) -> anyhow::Result<Vec<Guest>> {
    let mut out: Vec<Guest> = devices::list(&agent.db())?
        .into_iter()
        .filter_map(|d| {
            Some(Guest {
                ends_ms: d.expires_ms?,
                permission: d.permission.parse().unwrap_or(Permission::View),
                connected: d
                    .device_id
                    .parse::<PublicKey>()
                    .is_ok_and(|k| agent.is_connected(&k)),
                id: d.device_id,
                name: d.name,
                joined_ms: d.paired_at_ms,
            })
        })
        .collect();
    out.sort_by_key(|g| g.ends_ms);
    Ok(out)
}

/// Gives a guest more time, from its current end; at most a week from now.
pub fn extend(agent: &Agent, caller: &PublicKey, device: &str, minutes: u32) -> Result<(), String> {
    let d = devices::get(&agent.db(), device)
        .map_err(|e| format!("{e:#}"))?
        .ok_or("no such device on this machine")?;
    let Some(ends) = d.expires_ms else {
        return Err("that device isn't a guest".into());
    };
    // Full control guests from before the cap keep their time, but get no more.
    if d.permission.parse().unwrap_or(Permission::View) > MAX_LEVEL {
        return Err("a guest with Full control can't get more time".into());
    }
    let now = now_ms();
    let next =
        (ends.max(now) + i64::from(minutes) * 60_000).min(now + i64::from(MAX_MINUTES) * 60_000);
    devices::set_expiry(&agent.db(), device, Some(next)).map_err(|e| format!("{e:#}"))?;
    let _ = audit::record(
        &agent.db(),
        &caller.to_string(),
        Event::Invite,
        &d.name,
        &format!("{} more", for_words(minutes).trim_start_matches("for ")),
    );
    Ok(())
}

/// Removes guests whose time is up, closes their connections, and audits each (the
/// notice). Returns how many ended.
pub fn sweep(agent: &Agent) -> usize {
    let Ok(ended) = devices::ended(&agent.db(), now_ms()) else {
        return 0;
    };
    for d in &ended {
        // Audited first: the notice reads the device's name from its row.
        let _ = audit::record(
            &agent.db(),
            &d.device_id,
            Event::Revoke,
            &d.name,
            GUEST_ENDED,
        );
        let _ = devices::remove(&agent.db(), &d.device_id);
        if let Ok(k) = d.device_id.parse::<PublicKey>() {
            agent.pairing.invites.drop_by(&k);
            agent.drop_connections(&k);
        }
        // A restart, sleep or shut down the guest asked for doesn't happen.
        agent.power.cancel_for(agent, &d.device_id);
    }
    ended.len()
}

/// Sweeps while the agent runs: when the next guest's time is up, so its open streams
/// close then, and every [`SWEEP_EVERY`] besides.
pub fn start_sweeper(agent: Arc<Agent>) {
    tokio::spawn(async move {
        loop {
            sweep(&agent);
            let soonest = devices::list(&agent.db())
                .ok()
                .and_then(|all| all.iter().filter_map(|d| d.expires_ms).min());
            tokio::select! {
                _ = tokio::time::sleep(sleep_for(soonest, now_ms())) => {}
                _ = JOINED.notified() => {}
            }
        }
    });
}

/// How long the sweeper may sleep, given the soonest guest's end.
fn sleep_for(soonest_ms: Option<i64>, now_ms: i64) -> Duration {
    let Some(end) = soonest_ms else {
        return SWEEP_EVERY;
    };
    let left = Duration::from_millis(u64::try_from(end - now_ms).unwrap_or(0));
    left.clamp(SWEEP_AT_LEAST, SWEEP_EVERY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sweeper_wakes_when_the_next_guest_ends() {
        assert_eq!(sleep_for(None, 0), SWEEP_EVERY);
        assert_eq!(sleep_for(Some(3_000), 1_000), Duration::from_secs(2));
        assert_eq!(sleep_for(Some(60_000_000), 0), SWEEP_EVERY);
        // Ended already: soon, without spinning.
        assert_eq!(sleep_for(Some(5), 1_000), SWEEP_AT_LEAST);
    }
}
