//! The push when a guest first opens a shared model's link (F48, board "Share a model", slot
//! "When they start"): once per share, free like the share itself, sent even when job alerts
//! are off. Only who and which model; never anything the guest asks.

use anyhow::Result;
use pocketgpu_proto::push::{AlertKind, AlertRequest, BODY_MAX, TITLE_MAX, clip};

use crate::daemon::state::Agent;
use crate::share::store::Share;

/// The push's title and body; without the guest's name when alert text is hidden.
pub fn text(label: &str, model: &str, machine: &str, generic: bool) -> (String, String) {
    if generic {
        return (
            clip("Your shared model is in use", TITLE_MAX),
            clip(
                "Someone you shared a model with started chatting.",
                BODY_MAX,
            ),
        );
    }
    (
        clip(&format!("{label} started chatting"), TITLE_MAX),
        clip(
            &format!("{model} on {machine}. Tap to see your shares."),
            BODY_MAX,
        ),
    )
}

/// The ticket of the device that made the share; when it has none (a desktop), those of
/// every Full control device.
pub fn tickets(db: &rusqlite::Connection, made_by: &str) -> Result<Vec<String>> {
    let mine: Option<String> = db
        .query_row(
            "SELECT push_ticket FROM devices WHERE device_id = ?1 AND push_ticket IS NOT NULL",
            [made_by],
            |r| r.get(0),
        )
        .ok();
    if let Some(t) = mine {
        return Ok(vec![t]);
    }
    let mut stmt = db.prepare(
        "SELECT push_ticket FROM devices WHERE push_ticket IS NOT NULL AND permission = 'shell'",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Sends it; nothing when no device has a ticket.
pub async fn send(agent: &Agent, share: &Share) -> Result<u32> {
    let tickets = tickets(&agent.db(), &share.made_by)?;
    if tickets.is_empty() {
        return Ok(0);
    }
    let (generic, push_url) = {
        let cfg = agent.config.read().expect("config lock poisoned");
        (cfg.alerts.generic_text, cfg.push_url.clone())
    };
    let (title, body) = text(&share.label, &share.model, &agent.name(), generic);
    let alert = AlertRequest {
        tickets,
        kind: AlertKind::GuestStarted,
        title,
        body,
        link: Some(format!(
            "pocketgpu://machine/{}/shared/{}",
            agent.identity.agent_id(),
            share.id
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
    fn names_the_guest_or_hides_it() {
        let (t, b) = text("Mum", "llama3.2", "Mac mini", false);
        assert_eq!(t, "Mum started chatting");
        assert_eq!(b, "llama3.2 on Mac mini. Tap to see your shares.");
        let (t, b) = text("Mum", "llama3.2", "Mac mini", true);
        assert!(!t.contains("Mum") && !b.contains("Mum") && !b.contains("llama"));
    }

    #[test]
    fn goes_to_the_maker_or_else_full_control_devices() {
        let db = open_in_memory().unwrap();
        let up = |id: &str, p: Permission, t: Option<&str>| {
            devices::upsert(&db, id, id, Platform::Android, p, t).unwrap();
        };
        up("view", Permission::View, Some("t-view"));
        up("phone", Permission::Shell, Some("t-phone"));
        up("desk", Permission::Shell, None);
        assert_eq!(tickets(&db, "phone").unwrap(), vec!["t-phone".to_string()]);
        // Made on the desktop, which gets no pushes: the Full control phone does.
        assert_eq!(tickets(&db, "desk").unwrap(), vec!["t-phone".to_string()]);
    }
}
