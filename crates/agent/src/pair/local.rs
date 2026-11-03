//! The PocketGPU app on this same PC pairs itself over the local socket (F34), with no code.
//! Only this user can open the socket, and it already approves phones at any permission, so
//! this grants nothing new.

use iroh::PublicKey;
use pocketgpu_proto::{Permission, Platform};
use rusqlite::Connection;

use crate::audit::{self, Event};
use crate::store::devices;

/// Adds the app's key with Shell, or keeps an existing device as it is.
pub fn pair(db: &Connection, device: &str, name: &str) -> Result<(), String> {
    let key: PublicKey = device.parse().map_err(|_| "not a device key".to_string())?;
    let device = key.to_string();
    if name.trim().is_empty() {
        return Err("the name is empty".into());
    }
    let name = super::clean_name(name);
    let existing = devices::get(db, &device).map_err(|e| format!("{e:#}"))?;
    if existing.is_some() {
        return Ok(());
    }
    devices::upsert(
        db,
        &device,
        &name,
        Platform::Desktop,
        Permission::Shell,
        None,
    )
    .and_then(|()| devices::mark_this_pc(db, &device))
    .and_then(|()| audit::record(db, &device, Event::Pair, &name, "shell · on this PC"))
    .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::open_in_memory;

    fn key() -> String {
        iroh::SecretKey::generate().public().to_string()
    }

    #[test]
    fn a_new_app_gets_shell_and_an_audit_entry() {
        let db = open_in_memory().unwrap();
        let k = key();
        pair(&db, &k, "PocketGPU on this PC").unwrap();
        let d = devices::get(&db, &k).unwrap().unwrap();
        assert_eq!(d.permission, "shell");
        assert_eq!(d.platform, "desktop");
        assert_eq!(d.name, "PocketGPU on this PC");
        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM audit WHERE device_id = ?1 AND event = 'pair'",
                [&k],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_paired_app_keeps_a_lower_permission() {
        let db = open_in_memory().unwrap();
        let k = key();
        devices::upsert(&db, &k, "PC", Platform::Desktop, Permission::View, None).unwrap();
        pair(&db, &k, "PocketGPU on this PC").unwrap();
        let d = devices::get(&db, &k).unwrap().unwrap();
        assert_eq!(d.permission, "view");
        assert_eq!(d.name, "PC");
    }

    #[test]
    fn bad_keys_and_empty_names_are_refused() {
        let db = open_in_memory().unwrap();
        assert!(pair(&db, "abc", "PocketGPU").is_err());
        assert!(pair(&db, &key(), "  ").is_err());
        assert!(devices::list(&db).unwrap().is_empty());
    }
}
