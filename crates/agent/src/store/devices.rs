//! Paired phones (`devices` table).

use anyhow::Result;
use pocketgpu_proto::{Permission, Platform};
use rusqlite::{Connection, params};
use serde::Serialize;

use super::db::now_ms;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Device {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub permission: String,
    pub paired_at_ms: i64,
    pub last_seen_ms: Option<i64>,
}

pub fn platform_str(p: Platform) -> &'static str {
    match p {
        Platform::Ios => "ios",
        Platform::Android => "android",
        Platform::Desktop => "desktop",
        Platform::Unknown => "unknown",
    }
}

/// Adds or re-pairs a phone. Names are cut to 40 characters.
pub fn upsert(
    db: &Connection,
    device_id: &str,
    name: &str,
    platform: Platform,
    permission: Permission,
    push_ticket: Option<&str>,
) -> Result<()> {
    let name: String = name.chars().take(40).collect();
    db.execute(
        "INSERT INTO devices (device_id, name, platform, permission, push_ticket, paired_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(device_id) DO UPDATE SET name = ?2, platform = ?3, permission = ?4,
             push_ticket = coalesce(?5, push_ticket)",
        params![
            device_id,
            name,
            platform_str(platform),
            permission.as_str(),
            push_ticket,
            now_ms()
        ],
    )?;
    Ok(())
}

pub fn list(db: &Connection) -> Result<Vec<Device>> {
    let mut stmt = db.prepare(
        "SELECT device_id, name, platform, permission, paired_at, last_seen FROM devices ORDER BY paired_at",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Device {
            device_id: r.get(0)?,
            name: r.get(1)?,
            platform: r.get(2)?,
            permission: r.get(3)?,
            paired_at_ms: r.get(4)?,
            last_seen_ms: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn remove(db: &Connection, device_id: &str) -> Result<bool> {
    Ok(db.execute("DELETE FROM devices WHERE device_id = ?1", [device_id])? > 0)
}

pub fn set_permission(db: &Connection, device_id: &str, permission: Permission) -> Result<bool> {
    Ok(db.execute(
        "UPDATE devices SET permission = ?1 WHERE device_id = ?2",
        params![permission.as_str(), device_id],
    )? > 0)
}

pub fn push_tickets(db: &Connection) -> Result<Vec<String>> {
    let mut stmt = db.prepare("SELECT push_ticket FROM devices WHERE push_ticket IS NOT NULL")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The phone's push ticket from the push server; replaces the old one.
pub fn set_push_ticket(db: &Connection, device_id: &str, ticket: &str) -> Result<bool> {
    Ok(db.execute(
        "UPDATE devices SET push_ticket = ?1 WHERE device_id = ?2",
        params![ticket, device_id],
    )? == 1)
}

pub fn get(db: &Connection, device_id: &str) -> Result<Option<Device>> {
    Ok(list(db)?.into_iter().find(|d| d.device_id == device_id))
}
