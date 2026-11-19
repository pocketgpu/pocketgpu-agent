//! Being found on the same Wi-Fi (F47): the LAN key, the DNS-SD record, and pairing from the
//! Wi-Fi ([`pair`]). See `specs/F47-find-on-wifi/contracts/lan.md`.

pub mod pair;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use pocketgpu_proto::lan::{self, LanKey, Os, Record};
use pocketgpu_proto::msg::control::Response;
use pocketgpu_proto::{Error, ErrorCode};

use crate::daemon::state::Agent;
use crate::store::paths::{Paths, write_private};

/// How often the announcer looks at the settings again, besides being told.
const RECHECK: Duration = Duration::from_secs(20);

/// Wakes the announcer when `pgpu lan` changes the setting.
static CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Whether finding on the Wi-Fi is on, and whether a session waits.
pub fn status(agent: &Agent) -> pocketgpu_proto::local::LanStatus {
    pocketgpu_proto::local::LanStatus {
        on: agent.config.read().expect("config lock poisoned").lan,
        waiting: *agent.pairing.watch_waiting().borrow(),
    }
}

/// `pgpu lan on|off`: saved, audited, and applied at once.
pub fn set(agent: &Agent, on: bool) -> Result<()> {
    {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        let mut next = cfg.clone();
        next.lan = on;
        next.save(&agent.paths.config_file())?;
        *cfg = next;
    }
    CHANGED.notify_one();
    let _ = crate::audit::record(
        &agent.db(),
        "local",
        crate::audit::Event::LanAccess,
        "lan",
        if on { "on" } else { "off" },
    );
    Ok(())
}

/// The LAN key, made the first time it's needed. Only paired devices ever get it.
pub fn load_or_make_key(paths: &Paths) -> Result<LanKey> {
    let file = paths.lan_key_file();
    if let Ok(bytes) = std::fs::read(&file)
        && let Ok(key) = <LanKey>::try_from(bytes.as_slice())
    {
        return Ok(key);
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow::anyhow!("no random numbers: {e}"))?;
    write_private(&file, &key).context("could not save the LAN key")?;
    Ok(key)
}

/// The answer to `GetLanKey` (View).
pub fn key_reply(agent: &Agent) -> Response {
    let on = agent.config.read().expect("config lock poisoned").lan;
    match load_or_make_key(&agent.paths) {
        Ok(key) => Response::LanKey { key, on },
        Err(e) => Response::Error(Error::new(ErrorCode::Internal, format!("{e:#}"))),
    }
}

/// What this computer shows on the Wi-Fi now, or nothing when finding is off.
pub fn record_for(
    on: bool,
    waiting: bool,
    name: &str,
    id: &str,
    key: &LanKey,
    unix_secs: u64,
) -> Option<Record> {
    if !on {
        return None;
    }
    Some(if waiting {
        Record::Waiting {
            name: name.to_string(),
            os: Os::this(),
            id: id.to_string(),
        }
    } else {
        Record::Quiet {
            tag: lan::tag(key, lan::hour_of(unix_secs)),
        }
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 8 random hex characters: an instance name that says nothing about the computer.
fn random_instance() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Keeps the record up to date until the task is aborted at shutdown: a new one when a
/// session opens or closes, every hour (a new tag), and when `pgpu lan` changes the setting.
/// Records left behind expire by themselves on the network.
pub async fn announce(agent: Arc<Agent>, sockets: Vec<SocketAddr>) {
    let Some(port) = sockets
        .iter()
        .find(|s| s.is_ipv4())
        .or(sockets.first())
        .map(SocketAddr::port)
    else {
        tracing::warn!("not found on the Wi-Fi: the endpoint has no socket");
        return;
    };
    let key = match load_or_make_key(&agent.paths) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("not found on the Wi-Fi: {e:#}");
            return;
        }
    };
    let id = agent.identity.agent_id().to_string();
    let mut waiting = agent.pairing.watch_waiting();
    let mut daemon: Option<ServiceDaemon> = None;
    // The record now showing and its full name, to replace it only when it changes.
    let mut shown: Option<(Record, String)> = None;
    let mut quiet_instance = random_instance();

    loop {
        let on = agent.config.read().expect("config lock poisoned").lan;
        let is_waiting = *waiting.borrow_and_update();
        let want = record_for(on, is_waiting, &agent.name(), &id, &key, now_secs());

        if want.as_ref() != shown.as_ref().map(|(r, _)| r) {
            if let (Some(d), Some((_, full))) = (&daemon, shown.take()) {
                let _ = d.unregister(&full);
            }
            match want {
                None => {
                    if let Some(d) = daemon.take() {
                        let _ = d.shutdown();
                    }
                }
                Some(record) => {
                    if daemon.is_none() {
                        match ServiceDaemon::new() {
                            Ok(d) => {
                                // Loopback addresses mean nothing to a phone; keep them off the Wi-Fi.
                                // macOS also gives lo0 the link-local fe80::1, which the
                                // library's loopback switches leave in.
                                let _ = d.disable_interface(vec![
                                    mdns_sd::IfKind::LoopbackV4,
                                    mdns_sd::IfKind::LoopbackV6,
                                    #[cfg(target_os = "macos")]
                                    mdns_sd::IfKind::Name("lo0".into()),
                                ]);
                                daemon = Some(d)
                            }
                            Err(e) => tracing::warn!("not found on the Wi-Fi: {e}"),
                        }
                    }
                    // A new name for each session, so one session can't be linked to the next.
                    let instance = if matches!(record, Record::Waiting { .. }) {
                        random_instance()
                    } else {
                        quiet_instance.clone()
                    };
                    if let Some(d) = &daemon {
                        match register(d, &instance, port, &record) {
                            Ok(full) => shown = Some((record, full)),
                            Err(e) => {
                                tracing::warn!("could not show this computer on the Wi-Fi: {e:#}")
                            }
                        }
                    }
                }
            }
        }

        let to_next_hour = 3600 - now_secs() % 3600;
        tokio::select! {
            _ = CHANGED.notified() => {}
            changed = waiting.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep(RECHECK.min(Duration::from_secs(to_next_hour + 1))) => {}
        }
        if to_next_hour < RECHECK.as_secs() {
            // A quiet record gets a new name with each new tag, so hours can't be linked.
            quiet_instance = random_instance();
        }
    }
    if let Some(d) = daemon {
        if let Some((_, full)) = shown {
            let _ = d.unregister(&full);
        }
        let _ = d.shutdown();
    }
}

fn register(d: &ServiceDaemon, instance: &str, port: u16, record: &Record) -> Result<String> {
    let props: Vec<(&str, String)> = record.to_txt();
    // The host name is as anonymous as the instance: not the computer's own name.
    let host = format!("{instance}.local.");
    let info = ServiceInfo::new(
        lan::SERVICE_TYPE,
        instance,
        &host,
        (),
        port,
        props.as_slice(),
    )
    .context("bad record")?
    .enable_addr_auto();
    let full = info.get_fullname().to_string();
    d.register(info).context("mDNS refused the record")?;
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: LanKey = [3u8; 32];

    #[test]
    fn off_shows_nothing() {
        assert_eq!(record_for(false, true, "mini", "id", &KEY, 0), None);
        assert_eq!(record_for(false, false, "mini", "id", &KEY, 0), None);
    }

    #[test]
    fn the_name_shows_only_while_a_session_waits() {
        match record_for(true, true, "mini", "id", &KEY, 0) {
            Some(Record::Waiting { name, id, .. }) => {
                assert_eq!(name, "mini");
                assert_eq!(id, "id");
            }
            other => panic!("{other:?}"),
        }
        let quiet = record_for(true, false, "mini", "id", &KEY, 7200).unwrap();
        assert_eq!(
            quiet,
            Record::Quiet {
                tag: lan::tag(&KEY, 2)
            }
        );
        let txt = quiet.to_txt();
        assert!(txt.iter().all(|(_, v)| !v.contains("mini") && v != "id"));
    }

    #[test]
    fn the_key_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path().to_path_buf());
        paths.create_all().unwrap();
        let a = load_or_make_key(&paths).unwrap();
        let b = load_or_make_key(&paths).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, [0u8; 32]);
    }
}
