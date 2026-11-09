//! Files offered with `pgpu send` (F40 user story 3): kept for 7 days in the `offers`
//! table, for every device with Control or only the one named. A device saves one by its
//! id; the file must still have the size and time it had when it was offered. The devices
//! get a push that names this machine, never the file.

use std::path::{Path, PathBuf};

use anyhow::Result;
use pocketgpu_proto::files::Offer;
use pocketgpu_proto::push::{AlertKind, AlertRequest};
use rusqlite::{Connection, OptionalExtension, params};

use crate::daemon::state::Agent;

/// How long an offer stays.
pub const KEEP_MS: i64 = 7 * 24 * 3600 * 1000;

/// An offer as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified_ms: u64,
    /// `None`: every device with Control.
    pub to_device: Option<String>,
    pub created_ms: i64,
    pub expires_ms: i64,
}

impl Stored {
    pub fn to_proto(&self) -> Offer {
        Offer {
            id: self.id.clone(),
            name: self.name.clone(),
            size: self.size,
            offered_ms: self.created_ms.max(0) as u64,
            expires_ms: self.expires_ms.max(0) as u64,
        }
    }
}

impl Stored {
    /// A new offer of a canonical file path, from `now` for [`KEEP_MS`].
    pub fn new(
        path: &Path,
        name: &str,
        size: u64,
        modified_ms: u64,
        to_device: Option<&str>,
        now: i64,
    ) -> Result<Self> {
        Ok(Self {
            id: new_id()?,
            path: path.to_path_buf(),
            name: name.into(),
            size,
            modified_ms,
            to_device: to_device.map(str::to_string),
            created_ms: now,
            expires_ms: now + KEEP_MS,
        })
    }
}

/// Keeps a new offer.
pub fn insert(db: &Connection, offer: &Stored) -> Result<()> {
    let Some(text) = offer.path.to_str() else {
        anyhow::bail!("PocketGPU can't offer a path that isn't valid Unicode");
    };
    db.execute(
        "INSERT INTO offers (id, path, name, size, modified_ms, to_device, created_ms, expires_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            offer.id,
            text,
            offer.name,
            offer.size as i64,
            offer.modified_ms as i64,
            offer.to_device,
            offer.created_ms,
            offer.expires_ms
        ],
    )?;
    Ok(())
}

const COLUMNS: &str = "id, path, name, size, modified_ms, to_device, created_ms, expires_ms";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Stored> {
    Ok(Stored {
        id: r.get(0)?,
        path: PathBuf::from(r.get::<_, String>(1)?),
        name: r.get(2)?,
        size: r.get::<_, i64>(3)?.max(0) as u64,
        modified_ms: r.get::<_, i64>(4)?.max(0) as u64,
        to_device: r.get(5)?,
        created_ms: r.get(6)?,
        expires_ms: r.get(7)?,
    })
}

/// Offers a device may save now, newest first.
pub fn for_device(db: &Connection, device: &str, now: i64) -> Result<Vec<Stored>> {
    let mut stmt = db.prepare(&format!(
        "SELECT {COLUMNS} FROM offers
         WHERE expires_ms > ?1 AND (to_device IS NULL OR to_device = ?2)
         ORDER BY created_ms DESC, id"
    ))?;
    let rows = stmt.query_map(params![now, device], row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One offer, if this device may save it now.
pub fn find(db: &Connection, id: &str, device: &str, now: i64) -> Result<Option<Stored>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM offers
                 WHERE id = ?1 AND expires_ms > ?2 AND (to_device IS NULL OR to_device = ?3)"
            ),
            params![id, now, device],
            row,
        )
        .optional()?)
}

/// Offers that haven't expired.
pub fn count(db: &Connection, now: i64) -> Result<u32> {
    Ok(db.query_row(
        "SELECT count(*) FROM offers WHERE expires_ms > ?1",
        [now],
        |r| r.get(0),
    )?)
}

/// Removes expired offers; the files themselves stay where they are.
pub fn prune(db: &Connection, now: i64) -> Result<usize> {
    Ok(db.execute("DELETE FROM offers WHERE expires_ms <= ?1", [now])?)
}

/// 16 random hex characters.
fn new_id() -> Result<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no random numbers: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Push tickets of these devices, those that have one, with the device each is for.
pub fn tickets(db: &Connection, devices: &[String]) -> Result<Vec<(String, String)>> {
    let mut stmt = db.prepare(
        "SELECT push_ticket FROM devices WHERE device_id = ?1 AND push_ticket IS NOT NULL",
    )?;
    let mut out = Vec::new();
    for d in devices {
        if let Some(t) = stmt.query_row([d], |r| r.get(0)).optional()? {
            out.push((d.clone(), t));
        }
    }
    Ok(out)
}

/// The push: the machine's name only, never the file's.
pub fn push_text(machine: &str) -> (String, String) {
    use pocketgpu_proto::push::{BODY_MAX, TITLE_MAX, clip};
    (
        clip(&format!("{machine} has a file for you"), TITLE_MAX),
        clip("Open PocketGPU to save it.", BODY_MAX),
    )
}

/// Sends the push in the background: `pgpu send` doesn't wait for the push server. Free on
/// every plan, like a note (`AlertKind::JobEnd`).
pub fn push(agent: &std::sync::Arc<Agent>, tickets: Vec<String>) {
    if tickets.is_empty() {
        return;
    }
    let agent = agent.clone();
    tokio::spawn(async move {
        let push_url = agent
            .config
            .read()
            .expect("config lock poisoned")
            .push_url
            .clone();
        let (title, body) = push_text(&agent.name());
        let alert = AlertRequest {
            tickets,
            kind: AlertKind::JobEnd,
            title,
            body,
            link: Some(format!(
                "pocketgpu://machine/{}/files",
                agent.identity.agent_id()
            )),
        };
        let sent = match crate::alerts::client::http() {
            Ok(http) => crate::alerts::sender::post(&agent, &http, &push_url, &alert).await,
            Err(e) => Err(e),
        };
        match sent {
            Ok(r) => tracing::info!(phones = r.delivered, "file offer push sent"),
            Err(e) => tracing::warn!("file offer push not sent: {e:#}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::open_in_memory;
    use crate::store::devices;
    use pocketgpu_proto::{Permission, Platform};

    #[test]
    fn offers_reach_everyone_or_the_one_named_until_they_expire() {
        let db = open_in_memory().unwrap();
        let create = |path: &str, size, to, now| {
            let offer = Stored::new(Path::new(path), "f.bin", size, 9, to, now).unwrap();
            insert(&db, &offer).unwrap();
            offer
        };
        let all = create("/x/a.bin", 5, None, 1000);
        let one = create("/x/b.bin", 6, Some("p8"), 2000);
        assert_eq!(all.id.len(), 16);
        assert_ne!(all.id, one.id);

        let ids = |d: &str, now| -> Vec<String> {
            for_device(&db, d, now)
                .unwrap()
                .into_iter()
                .map(|o| o.id)
                .collect()
        };
        assert_eq!(ids("p8", 3000), vec![one.id.clone(), all.id.clone()]);
        assert_eq!(ids("ipad", 3000), vec![all.id.clone()]);
        assert!(find(&db, &one.id, "ipad", 3000).unwrap().is_none());
        assert_eq!(find(&db, &one.id, "p8", 3000).unwrap().unwrap(), one);
        assert_eq!(count(&db, 3000).unwrap(), 2);

        // Seven days later the first is gone, then pruned.
        let later = 1000 + KEEP_MS;
        assert_eq!(ids("p8", later), vec![one.id.clone()]);
        assert!(find(&db, &all.id, "p8", later).unwrap().is_none());
        assert_eq!(prune(&db, later).unwrap(), 1);
        assert_eq!(count(&db, later).unwrap(), 1);
        assert_eq!(one.to_proto().expires_ms, (2000 + KEEP_MS) as u64);
    }

    #[test]
    fn the_push_never_names_the_file() {
        let (title, body) = push_text("rig-01");
        assert_eq!(title, "rig-01 has a file for you");
        assert_eq!(body, "Open PocketGPU to save it.");
    }

    #[test]
    fn tickets_only_of_the_devices_given() {
        let db = open_in_memory().unwrap();
        let add = |id: &str, ticket: Option<&str>| {
            devices::upsert(&db, id, id, Platform::Android, Permission::Control, ticket).unwrap()
        };
        add("a", Some("t-a"));
        add("b", None);
        add("c", Some("t-c"));
        let got = tickets(&db, &["a".into(), "b".into()]).unwrap();
        assert_eq!(got, vec![("a".to_string(), "t-a".to_string())]);
    }
}
