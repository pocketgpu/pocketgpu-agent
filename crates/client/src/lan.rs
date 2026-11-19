//! Finding computers on the same Wi-Fi (F47): browsing their records, pairing with one that
//! waits for a phone, and recognising paired ones by their hourly tag.

use std::net::SocketAddr;

use iroh::{EndpointAddr, PublicKey};
use pocketgpu_proto::lan::{self, LanKey, LanPairRequest, Os, Record};
use pocketgpu_proto::{alpn, write_frame};

use crate::{Client, ClientError, PAIR_CONNECT_TIMEOUT, Pairing, PhoneInfo};

/// A computer seen on the Wi-Fi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// Waiting for a phone (`pgpu pair` runs there): shown in Found on this Wi-Fi.
    Waiting(Found),
    /// Not waiting: only its hourly tag, for paired devices to recognise.
    Quiet {
        fullname: String,
        tag: [u8; 8],
        addrs: Vec<SocketAddr>,
    },
    /// A record went away.
    Lost { fullname: String },
}

/// A computer that waits for a phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The record's full name, to match `Lost`.
    pub fullname: String,
    pub agent_id: PublicKey,
    pub name: String,
    pub os: Os,
    pub addrs: Vec<SocketAddr>,
}

impl Found {
    /// The key's last four characters, to tell two computers with the same name apart.
    pub fn short_key(&self) -> String {
        let s = self.agent_id.to_string();
        s[s.len().saturating_sub(4)..].to_string()
    }

    fn endpoint_addr(&self) -> EndpointAddr {
        self.addrs
            .iter()
            .fold(EndpointAddr::new(self.agent_id), |a, ip| {
                a.with_ip_addr(*ip)
            })
    }
}

/// Reads one record. Records without addresses, or with a bad id, are left out.
pub fn seen_from(fullname: &str, txt: &Record, addrs: Vec<SocketAddr>) -> Option<Seen> {
    if addrs.is_empty() {
        return None;
    }
    match txt {
        Record::Waiting { name, os, id } => Some(Seen::Waiting(Found {
            fullname: fullname.to_string(),
            agent_id: id.parse().ok()?,
            name: name.clone(),
            os: *os,
            addrs,
        })),
        Record::Quiet { tag } => Some(Seen::Quiet {
            fullname: fullname.to_string(),
            tag: *tag,
            addrs,
        }),
    }
}

/// Which saved computer a quiet record belongs to, if any: `machines` holds each paired
/// computer's id and LAN key.
pub fn recognise<'a, I>(tag: &[u8; 8], machines: I, now_hour: u64) -> Option<&'a str>
where
    I: IntoIterator<Item = (&'a str, &'a LanKey)>,
{
    machines
        .into_iter()
        .find(|(_, key)| lan::tag_matches(key, tag, now_hour))
        .map(|(id, _)| id)
}

impl Client {
    /// Asks a computer found on the Wi-Fi to pair. The replies come as for a code: `Pending`,
    /// then `Approved` or `Denied` once someone at the computer decides. The computer must
    /// answer with the key its record showed.
    pub async fn pair_lan(&self, found: &Found, info: PhoneInfo) -> Result<Pairing, ClientError> {
        let conn = self
            .dial_within(found.endpoint_addr(), alpn::PAIR_LAN, PAIR_CONNECT_TIMEOUT)
            .await?;
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        let req = LanPairRequest {
            device_name: info.device_name,
            platform: info.platform,
            push_ticket: info.push_ticket,
        };
        write_frame(&mut send, &req).await?;
        send.finish()
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        Ok(Pairing {
            conn,
            recv,
            expected: Some(found.agent_id),
        })
    }
}

#[cfg(not(target_os = "ios"))]
pub use browse::{Browser, browse};

/// Browsing with `mdns-sd`. The iPhone browses with Apple's Bonjour instead (a Tauri plugin)
/// and hands its results to [`seen_from`].
#[cfg(not(target_os = "ios"))]
mod browse {
    use std::collections::HashMap;
    use std::net::{SocketAddr, SocketAddrV6};

    use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent};
    use pocketgpu_proto::lan::{Record, SERVICE_TYPE};
    use tokio::sync::mpsc;

    use super::{Seen, seen_from};
    use crate::ClientError;

    /// Browsing in progress; it stops when dropped.
    pub struct Browser {
        daemon: ServiceDaemon,
    }

    impl Drop for Browser {
        fn drop(&mut self) {
            let _ = self.daemon.stop_browse(SERVICE_TYPE);
            let _ = self.daemon.shutdown();
        }
    }

    /// The addresses a phone can dial, IPv4 first: no loopback, and IPv6 link-local ones only
    /// with the network they were heard on (they can't be dialled without it). The library
    /// sends a computer again as more of its addresses arrive, each time with all it knows.
    pub(super) fn usable<'a>(
        ips: impl Iterator<Item = &'a ScopedIp>,
        port: u16,
    ) -> Vec<SocketAddr> {
        let mut out: Vec<SocketAddr> = ips
            .filter(|a| !a.is_loopback())
            .filter_map(|a| match a {
                ScopedIp::V4(v4) => Some(SocketAddr::new((*v4.addr()).into(), port)),
                ScopedIp::V6(v6) => {
                    let scope = v6.scope_id().index;
                    let link_local = v6.addr().is_unicast_link_local();
                    if link_local && scope == 0 {
                        return None;
                    }
                    let scope = if link_local { scope } else { 0 };
                    Some(SocketAddrV6::new(*v6.addr(), port, 0, scope).into())
                }
                _ => None,
            })
            .collect();
        out.sort_by_key(|a| a.is_ipv6());
        out.dedup();
        out
    }

    /// Starts browsing; what is seen comes on the receiver until the [`Browser`] is dropped.
    pub fn browse() -> Result<(Browser, mpsc::Receiver<Seen>), ClientError> {
        let daemon = ServiceDaemon::new().map_err(|e| ClientError::Connect(e.to_string()))?;
        let events = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| ClientError::Connect(e.to_string()))?;
        let (tx, rx) = mpsc::channel(32);
        std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                let seen = match event {
                    ServiceEvent::ServiceResolved(s) => {
                        let props: HashMap<String, String> = s
                            .txt_properties
                            .iter()
                            .map(|p| (p.key().to_string(), p.val_str().to_string()))
                            .collect();
                        let Some(record) = Record::from_txt(|k| props.get(k).map(String::as_str))
                        else {
                            continue;
                        };
                        let addrs = usable(s.addresses.iter(), s.port);
                        seen_from(&s.fullname, &record, addrs)
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => Some(Seen::Lost { fullname }),
                    ServiceEvent::SearchStopped(_) => break,
                    _ => None,
                };
                if let Some(seen) = seen
                    && tx.blocking_send(seen).is_err()
                {
                    break;
                }
            }
        });
        Ok((Browser { daemon }, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_id() -> String {
        iroh::SecretKey::generate().public().to_string()
    }

    #[test]
    fn records_without_addresses_or_with_bad_ids_are_left_out() {
        let w = Record::Waiting {
            name: "mini".into(),
            os: Os::Mac,
            id: key_id(),
        };
        let addr: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        assert!(seen_from("a", &w, vec![]).is_none());
        assert!(matches!(
            seen_from("a", &w, vec![addr]),
            Some(Seen::Waiting(_))
        ));
        let bad = Record::Waiting {
            name: "mini".into(),
            os: Os::Mac,
            id: "not a key".into(),
        };
        assert!(seen_from("a", &bad, vec![addr]).is_none());
    }

    #[cfg(not(target_os = "ios"))]
    #[test]
    fn only_dialable_addresses_are_kept_ipv4_first() {
        use mdns_sd::{InterfaceId, ScopedIp, ScopedIpV4};
        let en1 = InterfaceId {
            name: "en1".into(),
            index: 15,
        };
        let v6 = |s: &str| ScopedIp::from(s.parse::<std::net::IpAddr>().unwrap());
        let ips = [
            v6("fe80::1"),
            v6("::1"),
            ScopedIp::V4(ScopedIpV4::new([127, 0, 0, 1].into(), en1.clone())),
            ScopedIp::V4(ScopedIpV4::new([192, 168, 1, 10].into(), en1)),
            v6("fd00::5"),
        ];
        let got: Vec<String> = browse::usable(ips.iter(), 7000)
            .iter()
            .map(|a| a.to_string())
            .collect();
        assert_eq!(got, ["192.168.1.10:7000", "[fd00::5]:7000"]);
    }

    #[test]
    fn a_quiet_record_is_recognised_only_with_its_key() {
        let k1 = [1u8; 32];
        let k2 = [2u8; 32];
        let tag = lan::tag(&k2, 500);
        let machines = [("one", &k1), ("two", &k2)];
        assert_eq!(recognise(&tag, machines, 500), Some("two"));
        assert_eq!(recognise(&tag, machines, 501), Some("two"));
        assert_eq!(recognise(&tag, machines, 502), None);
        assert_eq!(recognise(&tag, [("one", &k1)], 500), None);
    }
}
