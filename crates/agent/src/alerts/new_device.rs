//! The push to a machine's other devices when a new one pairs (F35, board 30 E.5): free on
//! every plan, sent even when job alerts are off, carrying only the two names.

use anyhow::Result;
use pocketgpu_proto::push::{AlertKind, AlertRequest, BODY_MAX, TITLE_MAX, clip};

use crate::daemon::state::Agent;

/// The push's title and body; without the device's name when alert text is hidden.
pub fn text(machine: &str, device: &str, permission: &str, generic: bool) -> (String, String) {
    let title = clip(&format!("New device paired with {machine}"), TITLE_MAX);
    let body = if generic {
        "A new device can now connect. Not you? Tap to revoke it.".to_string()
    } else {
        let access = match permission {
            "shell" => "Shell",
            "control" => "Control",
            _ => "View",
        };
        format!("{device} can now connect, with {access} access. Not you? Tap to revoke it.")
    };
    (title, clip(&body, BODY_MAX))
}

/// Push tickets of every device but the new one.
pub fn tickets(db: &rusqlite::Connection, new_device: &str) -> Result<Vec<String>> {
    let mut stmt = db.prepare(
        "SELECT push_ticket FROM devices WHERE push_ticket IS NOT NULL AND device_id != ?1",
    )?;
    let rows = stmt.query_map([new_device], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Sends it; nothing when no other device has a ticket (the first pairing).
pub async fn send(agent: &Agent, device_id: &str, name: &str, permission: &str) -> Result<u32> {
    let tickets = tickets(&agent.db(), device_id)?;
    if tickets.is_empty() {
        return Ok(0);
    }
    let (generic, push_url) = {
        let cfg = agent.config.read().expect("config lock poisoned");
        (cfg.alerts.generic_text, cfg.push_url.clone())
    };
    let (title, body) = text(&agent.name(), name, permission, generic);
    let alert = AlertRequest {
        tickets,
        kind: AlertKind::NewDevice,
        title,
        body,
        link: Some(format!(
            "pocketgpu://machine/{}/devices?new={device_id}",
            agent.identity.agent_id()
        )),
    };
    let http = super::client::http()?;
    let result = super::sender::post(agent, &http, &push_url, &alert).await?;
    Ok(result.delivered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{db::open_in_memory, devices};
    use pocketgpu_proto::{Permission, Platform};

    #[test]
    fn names_the_device_or_hides_it() {
        let (t, b) = text("rig-1", "Galaxy S24", "shell", false);
        assert_eq!(t, "New device paired with rig-1");
        assert!(b.starts_with("Galaxy S24 can now connect, with Shell access."));
        let (_, b) = text("rig-1", "Galaxy S24", "shell", true);
        assert!(!b.contains("Galaxy"));
    }

    #[test]
    fn goes_to_every_other_device_only() {
        let db = open_in_memory().unwrap();
        devices::upsert(
            &db,
            "a",
            "Pixel 8",
            Platform::Android,
            Permission::Shell,
            Some("t-a"),
        )
        .unwrap();
        devices::upsert(&db, "b", "iPad", Platform::Ios, Permission::View, None).unwrap();
        // The first device has nobody to tell.
        assert!(tickets(&db, "a").unwrap().is_empty());
        devices::upsert(
            &db,
            "c",
            "Galaxy",
            Platform::Android,
            Permission::Shell,
            Some("t-c"),
        )
        .unwrap();
        assert_eq!(tickets(&db, "c").unwrap(), vec!["t-a".to_string()]);
    }
}
