//! `pgpu/files/1` (F40): one request per bidirectional stream. Every request is checked
//! against the device's permission as it is now: View for job logs, Control for shared
//! folders, the Inbox and offers, Shell (and the terminal switch) for everything else.
//! Each device runs at most 4 transfers at once, and on the default relays big files wait
//! for a direct link (FR-007).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use pocketgpu_proto::files::{
    Folder, OVERVIEW_OFFERS, Overview, RELAY_LIMIT, Request, Response, Root,
};
use pocketgpu_proto::{Error, ErrorCode, FrameError, Permission, read_frame, write_frame};

use super::still::Still;
use super::{browse, get, inbox, offers};
use crate::daemon::auth::{Peer, PeerProtocol, until_closed};
use crate::daemon::state::Agent;

/// Gets and puts one device may run at once.
pub const MAX_TRANSFERS: u32 = 4;

#[derive(Debug, Clone, Copy)]
pub struct Files;

impl PeerProtocol for Files {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let (agent, peer, closed) = (agent.clone(), peer.clone(), conn.clone());
            tokio::spawn(until_closed(closed.clone(), async move {
                let reply = match read_frame::<_, Request>(&mut recv).await {
                    Ok(Some(req)) => {
                        handle(&agent, &peer, &closed, req, &mut send, &mut recv).await
                    }
                    Ok(None) => return,
                    Err(e) => Some(Response::Error(Error::new(
                        ErrorCode::Invalid,
                        e.to_string(),
                    ))),
                };
                if let Some(reply) = reply
                    && let Err(e @ (FrameError::TooLarge(_) | FrameError::Codec(_))) =
                        write_frame(&mut send, &reply).await
                {
                    // Nothing went yet: say why instead of ending with no reply.
                    let why = Error::new(ErrorCode::Internal, format!("no reply: {e}"));
                    let _ = write_frame(&mut send, &Response::Error(why)).await;
                }
                let _ = send.finish();
            }));
        }
    }
}

pub fn enabled(agent: &Agent) -> bool {
    agent.config.read().expect("config lock poisoned").files
}

pub fn off() -> Response {
    Response::Error(off_error())
}

pub fn off_error() -> Error {
    Error::new(
        ErrorCode::Forbidden,
        "files are off on this machine. Turn them on there with: pgpu files on",
    )
}

/// The last reply, or `None` when the handler already sent everything (a `Get` or `Put`).
async fn handle(
    agent: &Arc<Agent>,
    peer: &Peer,
    conn: &Connection,
    req: Request,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Option<Response> {
    if let Err(e) = agent.require(peer, req.min_permission(), &describe(&req)) {
        return Some(Response::Error(e));
    }
    let device = peer.key.to_string();
    if !enabled(agent) {
        return Some(match req {
            Request::Overview => Response::Overview(off_overview(agent)),
            _ => off(),
        });
    }
    match req {
        Request::Overview => {
            let permission = agent.permission_of(&peer.key);
            let agent = agent.clone();
            let built =
                tokio::task::spawn_blocking(move || overview(&agent, &device, permission)).await;
            Some(match built {
                Ok(o) => Response::Overview(o),
                Err(e) => Response::Error(Error::new(ErrorCode::Internal, e.to_string())),
            })
        }
        Request::List { root, path, offset } => {
            Some(browse::list(agent, &device, root, path, offset).await)
        }
        Request::Get { root, path, from } => {
            let Some(_slot) = Slot::take(&device) else {
                return Some(Response::Error(too_many()));
            };
            let opened = match get::open_file(agent, &device, root, path).await {
                Ok(o) => o,
                Err(e) => return Some(Response::Error(e)),
            };
            if let Some(e) = relay_guard(agent, conn, opened.size()) {
                return Some(Response::Error(e));
            }
            let still = Still::get(peer.key, root, opened.shared.clone());
            get::send_file(agent, &device, opened, from, still, send).await
        }
        Request::Put {
            name,
            size,
            transfer,
        } => {
            if let Some(e) = relay_guard(agent, conn, size) {
                return Some(Response::Error(e));
            }
            let Some(_slot) = Slot::take(&device) else {
                return Some(Response::Error(too_many()));
            };
            let sent = inbox::Sent {
                name,
                size,
                transfer,
            };
            inbox::put(agent, peer.key, sent, send, recv).await
        }
    }
}

/// What a device sees while files are off: only that.
fn off_overview(agent: &Agent) -> Overview {
    Overview {
        enabled: false,
        shared: Vec::new(),
        inbox: String::new(),
        inbox_files: 0,
        job_logs: 0,
        offers: Vec::new(),
        home: false,
        relay_limit: relay_limit(agent),
    }
}

fn overview(agent: &Agent, device: &str, permission: Option<Permission>) -> Overview {
    let shared = agent
        .config
        .read()
        .expect("config lock poisoned")
        .files_shared
        .clone();
    let shared = shared
        .iter()
        .map(|p| Folder {
            name: p
                .file_name()
                .map_or_else(|| super::shown(p), |n| n.to_string_lossy().into_owned()),
            path: super::shown(p),
            files: super::count_files(p),
        })
        .collect();
    let inbox = super::inbox_dir(agent);
    let control = permission.is_some_and(|p| p >= Permission::Control);
    let offers = if control {
        offers::for_device(&agent.db(), device, crate::store::db::now_ms())
            .map(|list| {
                list.iter()
                    .take(OVERVIEW_OFFERS)
                    .map(offers::Stored::to_proto)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Overview {
        enabled: true,
        shared,
        inbox: super::shown(&inbox),
        inbox_files: super::count_files(&inbox),
        job_logs: browse::job_logs(&agent.paths.jobs_dir()).len() as u32,
        offers,
        home: permission == Some(Permission::Shell) && super::terminal_on(agent).is_ok(),
        relay_limit: relay_limit(agent),
    }
}

fn relay_limit(agent: &Agent) -> Option<u64> {
    let custom = !agent
        .config
        .read()
        .expect("config lock poisoned")
        .relays
        .is_empty();
    (!custom).then_some(RELAY_LIMIT)
}

/// FR-007: on the default relays, a file over [`RELAY_LIMIT`] waits while the connection
/// goes through a relay; the app tries again once the link is direct.
pub fn relay_guard(agent: &Agent, conn: &Connection, size: u64) -> Option<Error> {
    let custom = relay_limit(agent).is_none();
    let on_relay = conn
        .paths()
        .iter()
        .find(|p| p.is_selected())
        .is_some_and(|p| p.is_relay());
    relay_refusal(custom, on_relay, size)
}

pub fn relay_refusal(custom_relays: bool, on_relay: bool, size: u64) -> Option<Error> {
    (!custom_relays && on_relay && size > RELAY_LIMIT).then(|| {
        Error::new(
            ErrorCode::Busy,
            "files over 50 MB wait for a direct link to this machine instead of going \
             through PocketGPU's relays; it starts as soon as there is one",
        )
    })
}

/// Transfers running per device.
static TRANSFERS: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Mutex::default);

/// One of a device's [`MAX_TRANSFERS`], given back when dropped (also when the connection
/// closes mid-transfer).
pub struct Slot(String);

impl Slot {
    pub fn take(device: &str) -> Option<Self> {
        let mut map = TRANSFERS.lock().expect("transfers lock poisoned");
        let n = map.entry(device.to_string()).or_insert(0);
        if *n >= MAX_TRANSFERS {
            return None;
        }
        *n += 1;
        Some(Self(device.to_string()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut map = TRANSFERS.lock().expect("transfers lock poisoned");
        if let Some(n) = map.get_mut(&self.0) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&self.0);
            }
        }
    }
}

fn too_many() -> Error {
    Error::new(
        ErrorCode::Busy,
        "4 files are already on their way to or from this device; the next starts when one \
         is done",
    )
}

/// What a refused request was, for the audit log.
fn describe(req: &Request) -> String {
    match req {
        Request::Overview => "Files Overview".into(),
        Request::List { root, path, .. } => format!("Files List {} {path}", root_name(*root)),
        Request::Get { root, path, .. } => format!("Files Get {} {path}", root_name(*root)),
        Request::Put { name, size, .. } => format!("Files Put {name} ({size} bytes)"),
    }
}

pub fn root_name(root: Root) -> String {
    match root {
        Root::Shared(i) => format!("shared {i}"),
        Root::Inbox => "Inbox".into(),
        Root::JobLogs => "job logs".into(),
        Root::Offers => "offers".into(),
        Root::Home => "home".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_files_wait_for_a_direct_link_on_default_relays() {
        let big = RELAY_LIMIT + 1;
        let refused = relay_refusal(false, true, big).unwrap();
        assert_eq!(refused.code, ErrorCode::Busy);
        assert!(
            refused.message.contains("direct link"),
            "{}",
            refused.message
        );
        assert!(relay_refusal(false, true, RELAY_LIMIT).is_none());
        assert!(relay_refusal(false, false, big).is_none());
        assert!(relay_refusal(true, true, big).is_none());
    }

    #[test]
    fn four_transfers_per_device() {
        let slots: Vec<_> = (0..MAX_TRANSFERS)
            .map(|_| Slot::take("slots-test").unwrap())
            .collect();
        assert!(Slot::take("slots-test").is_none());
        assert!(Slot::take("slots-other").is_some());
        drop(slots);
        assert!(Slot::take("slots-test").is_some());
        assert!(!TRANSFERS.lock().unwrap().contains_key("slots-test"));
    }
}
