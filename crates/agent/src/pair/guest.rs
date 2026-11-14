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

use super::invite::{Grant, MAX_MINUTES, for_words, new_code};
use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;
use crate::store::devices;

/// The audit result when a guest's time ran out; shows the "access ended" notice.
pub const GUEST_ENDED: &str = "guest ended";
/// How often ended guests are looked for.
const SWEEP_EVERY: Duration = Duration::from_secs(15);

/// A new invite from `by` (a device with Shell): the `pocketgpu://pair?…` link and when the
/// access would end. The words in the link (who, what, until) are for the joining phone's
/// screen only; the computer keeps the real ones.
pub async fn invite(
    agent: &Arc<Agent>,
    by: &PublicKey,
    permission: Permission,
    minutes: u32,
) -> Result<(String, i64), String> {
    if minutes == 0 || minutes > MAX_MINUTES {
        return Err("a guest can have the computer from a minute up to a week".into());
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

/// Pairs a device that proved an invite, with the invite's level and end time.
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
            agent.drop_connections(&k);
        }
    }
    ended.len()
}

/// Sweeps every few seconds while the agent runs.
pub fn start_sweeper(agent: Arc<Agent>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            sweep(&agent);
        }
    });
}
