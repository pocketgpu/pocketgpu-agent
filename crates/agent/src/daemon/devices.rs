//! Managing paired phones (F03): list, change a permission, revoke. Permissions change only
//! at the machine; a phone may only remove itself (`Unpair`). Every change is audited.

use std::time::Duration;

use iroh::PublicKey;
use pocketgpu_proto::Permission;
use pocketgpu_proto::local::DeviceInfo;

use super::state::Agent;
use crate::audit::{self, Event};
use crate::store::devices::{self, Device};

/// How long an unpairing phone gets to read the reply before its connection closes.
const UNPAIR_CLOSE_AFTER: Duration = Duration::from_millis(300);

fn key_of(d: &Device) -> Option<PublicKey> {
    d.device_id.parse().ok()
}

fn info(agent: &Agent, d: &Device) -> DeviceInfo {
    DeviceInfo {
        id: d.device_id.clone(),
        name: d.name.clone(),
        platform: d.platform.clone(),
        permission: d.permission.parse().unwrap_or(Permission::View),
        paired_at_ms: d.paired_at_ms,
        last_seen_ms: d.last_seen_ms,
        connected: key_of(d).is_some_and(|k| agent.is_connected(&k)),
    }
}

pub fn list(agent: &Agent) -> anyhow::Result<Vec<DeviceInfo>> {
    let all = devices::list(&agent.db())?;
    Ok(all.iter().map(|d| info(agent, d)).collect())
}

/// Finds one phone by its name (any case) or the start of its key (at least 4 characters).
pub fn find(agent: &Agent, query: &str) -> Result<Device, String> {
    let all = devices::list(&agent.db()).map_err(|e| format!("{e:#}"))?;
    let q = query.trim();
    let by_name: Vec<_> = all
        .iter()
        .filter(|d| d.name.eq_ignore_ascii_case(q))
        .collect();
    let by_key: Vec<_> = if q.len() >= 4 {
        let q = q.to_ascii_lowercase();
        all.iter().filter(|d| d.device_id.starts_with(&q)).collect()
    } else {
        Vec::new()
    };
    // Phones choose their names: one named like another's key must not get its changes.
    // Both kinds of match together count as ambiguous.
    let mut matches = by_name;
    for d in by_key {
        if !matches.iter().any(|m| m.device_id == d.device_id) {
            matches.push(d);
        }
    }
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!(
            "no paired phone called {query:?}; `pgpu devices` lists them"
        )),
        many => Err(format!(
            "{} phones match {query:?}; use the start of the key instead: {}",
            many.len(),
            many.iter()
                .map(|d| &d.device_id[..8])
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// `device` is who asked: `local` for the command line.
pub fn set_permission(
    agent: &Agent,
    query: &str,
    permission: Permission,
) -> Result<DeviceInfo, String> {
    let d = find(agent, query)?;
    devices::set_permission(&agent.db(), &d.device_id, permission).map_err(|e| format!("{e:#}"))?;
    let result = format!("{} -> {}", d.permission, permission.as_str());
    let _ = audit::record(
        &agent.db(),
        "local",
        Event::PermissionChange,
        &d.name,
        &result,
    );
    // Requests are checked against the stored permission, so open connections pick the
    // change up on their next request.
    let fresh = devices::get(&agent.db(), &d.device_id)
        .ok()
        .flatten()
        .unwrap_or(d);
    Ok(info(agent, &fresh))
}

/// Removes a phone and closes its connections at once.
pub fn revoke(agent: &Agent, query: &str) -> Result<DeviceInfo, String> {
    let d = find(agent, query)?;
    let before = info(agent, &d);
    devices::remove(&agent.db(), &d.device_id).map_err(|e| format!("{e:#}"))?;
    if let Some(k) = key_of(&d) {
        agent.drop_connections(&k);
    }
    let _ = audit::record(&agent.db(), "local", Event::Revoke, &d.name, "revoked");
    Ok(before)
}

/// A phone removing itself. The reply goes out before the connection closes.
pub fn unpair(agent: &std::sync::Arc<Agent>, key: PublicKey) -> anyhow::Result<bool> {
    let id = key.to_string();
    let name = devices::get(&agent.db(), &id)?.map(|d| d.name);
    let removed = devices::remove(&agent.db(), &id)?;
    if removed {
        let _ = audit::record(
            &agent.db(),
            &id,
            Event::Revoke,
            name.as_deref().unwrap_or("phone"),
            "unpaired itself",
        );
        let agent = agent.clone();
        tokio::spawn(async move {
            tokio::time::sleep(UNPAIR_CLOSE_AFTER).await;
            agent.drop_connections(&key);
        });
    }
    Ok(removed)
}
