//! Finding a computer on the same Wi-Fi (F47): what it shows there, and pairing without a code.
//!
//! A computer shows one DNS-SD record of [`SERVICE_TYPE`]. While a pairing session is open it
//! is [`Record::Waiting`] (name, system, endpoint id): anyone on the network may see it, as
//! they could see the session's QR code on the screen. Otherwise it is [`Record::Quiet`]: an
//! hourly [`tag`] keyed with the computer's LAN key, which only its paired devices hold, so
//! others can't tell which computer it is. See `specs/F47-find-on-wifi/contracts/lan.md`.

use serde::{Deserialize, Serialize};

use crate::types::Platform;

/// The DNS-SD service type, with the domain `mdns-sd` expects.
pub const SERVICE_TYPE: &str = "_pgpu._udp.local.";
/// The record format version.
pub const VERSION: &str = "1";
/// The longest name a waiting record carries.
pub const MAX_NAME: usize = 32;

const TAG_CONTEXT: &[u8] = b"pgpu lan tag";
const SECONDS_PER_HOUR: u64 = 3600;

/// A computer's LAN key: 32 random bytes, made once, given only to paired devices.
pub type LanKey = [u8; 32];

/// The hour number for a Unix time in seconds.
pub fn hour_of(unix_secs: u64) -> u64 {
    unix_secs / SECONDS_PER_HOUR
}

/// The quiet record's tag for one hour: the first 8 bytes of BLAKE3 keyed with the LAN key.
pub fn tag(key: &LanKey, hour: u64) -> [u8; 8] {
    let mut h = blake3::Hasher::new_keyed(key);
    h.update(TAG_CONTEXT);
    h.update(&hour.to_be_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&h.finalize().as_bytes()[..8]);
    out
}

/// Whether `seen` is this computer's tag for this hour or the one before (clocks and the
/// hourly change don't line up exactly).
pub fn tag_matches(key: &LanKey, seen: &[u8; 8], now_hour: u64) -> bool {
    *seen == tag(key, now_hour) || (now_hour > 0 && *seen == tag(key, now_hour - 1))
}

/// The computer's system, as a waiting record shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub fn as_str(self) -> &'static str {
        match self {
            Os::Mac => "mac",
            Os::Windows => "win",
            Os::Linux => "linux",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mac" => Some(Os::Mac),
            "win" => Some(Os::Windows),
            "linux" => Some(Os::Linux),
            _ => None,
        }
    }

    /// The system this program was built for.
    pub fn this() -> Self {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// What a computer shows on the Wi-Fi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// A pairing session is open: shown in Found on this Wi-Fi.
    Waiting {
        name: String,
        os: Os,
        /// The agent's endpoint id (its public key, as iroh prints it).
        id: String,
    },
    /// No session: only paired devices can tell which computer this is.
    Quiet { tag: [u8; 8] },
}

impl Record {
    /// The TXT properties, as key and value pairs.
    pub fn to_txt(&self) -> Vec<(&'static str, String)> {
        match self {
            Record::Waiting { name, os, id } => vec![
                ("v", VERSION.into()),
                ("p", "1".into()),
                ("n", clip_name(name)),
                ("os", os.as_str().into()),
                ("id", id.clone()),
            ],
            Record::Quiet { tag } => vec![("v", VERSION.into()), ("t", hex(tag))],
        }
    }

    /// Reads TXT properties. Records of another version, or missing a field, are `None`.
    pub fn from_txt<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Option<Self> {
        if get("v")? != VERSION {
            return None;
        }
        if get("p") == Some("1") {
            let name = get("n")?.trim();
            if name.is_empty() || name.chars().count() > MAX_NAME {
                return None;
            }
            let id = get("id")?;
            if id.is_empty() || id.len() > 128 {
                return None;
            }
            return Some(Record::Waiting {
                name: name.to_string(),
                os: Os::parse(get("os")?)?,
                id: id.to_string(),
            });
        }
        let t = get("t")?;
        let mut tag = [0u8; 8];
        unhex(t, &mut tag)?;
        Some(Record::Quiet { tag })
    }
}

/// What a phone sends on `pgpu/pair-lan/1`. The replies are `msg::pair::Response`, as for a
/// code: `Pending`, then `Approved`, `Denied` or `Error` (`Busy`: another request is waiting;
/// `Forbidden`: too many refusals, or nothing to pair with).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LanPairRequest {
    pub device_name: String,
    pub platform: Platform,
    pub push_ticket: Option<String>,
}

fn clip_name(name: &str) -> String {
    name.trim().chars().take(MAX_NAME).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str, out: &mut [u8]) -> Option<()> {
    if s.len() != out.len() * 2 {
        return None;
    }
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const KEY: LanKey = [7u8; 32];

    fn read(txt: &[(&'static str, String)]) -> Option<Record> {
        let m: HashMap<&str, &str> = txt.iter().map(|(k, v)| (*k, v.as_str())).collect();
        Record::from_txt(|k| m.get(k).copied())
    }

    #[test]
    fn the_tag_changes_every_hour() {
        assert_ne!(tag(&KEY, 100), tag(&KEY, 101));
        assert_eq!(tag(&KEY, 100), tag(&KEY, 100));
        assert_eq!(hour_of(3600 * 5 + 10), 5);
    }

    #[test]
    fn this_hour_and_the_last_match_but_not_older_ones() {
        let t = tag(&KEY, 100);
        assert!(tag_matches(&KEY, &t, 100));
        assert!(tag_matches(&KEY, &t, 101));
        assert!(!tag_matches(&KEY, &t, 102));
        assert!(!tag_matches(&KEY, &t, 99));
    }

    #[test]
    fn a_wrong_key_never_matches() {
        let t = tag(&KEY, 100);
        let other = [8u8; 32];
        assert!(!tag_matches(&other, &t, 100));
        assert!(!tag_matches(&other, &t, 101));
    }

    #[test]
    fn records_round_trip() {
        let w = Record::Waiting {
            name: "mini".into(),
            os: Os::Mac,
            id: "abc123".into(),
        };
        assert_eq!(read(&w.to_txt()), Some(w));
        let q = Record::Quiet { tag: tag(&KEY, 42) };
        assert_eq!(read(&q.to_txt()), Some(q));
    }

    #[test]
    fn a_quiet_record_says_nothing_about_the_computer() {
        let txt = Record::Quiet { tag: tag(&KEY, 42) }.to_txt();
        let keys: Vec<&str> = txt.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["v", "t"]);
    }

    #[test]
    fn long_names_are_cut_and_odd_records_refused() {
        let w = Record::Waiting {
            name: "x".repeat(50),
            os: Os::Linux,
            id: "id".into(),
        };
        match read(&w.to_txt()) {
            Some(Record::Waiting { name, .. }) => assert_eq!(name.len(), MAX_NAME),
            other => panic!("{other:?}"),
        }
        assert_eq!(read(&[("v", "2".into()), ("t", "00".repeat(8))]), None);
        assert_eq!(read(&[("v", "1".into()), ("t", "zz".into())]), None);
        assert_eq!(
            read(&[
                ("v", "1".into()),
                ("p", "1".into()),
                ("n", "mini".into()),
                ("os", "beos".into()),
                ("id", "x".into())
            ]),
            None
        );
    }
}
