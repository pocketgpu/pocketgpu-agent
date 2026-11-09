//! `pgpu/clip/1` (F41): one request and one reply per bidirectional stream. Putting text
//! needs Control and reading it Shell, checked against the device's permission as it is
//! now. Every put and read is audited; the text never is.

use std::sync::Arc;

use iroh::endpoint::Connection;
use pocketgpu_proto::clip::{Place, Request, Response};
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};

use super::Found;
use super::system::Read;
use crate::audit::{self, Event};
use crate::daemon::auth::{Peer, PeerProtocol, until_closed};
use crate::daemon::state::Agent;

#[derive(Debug, Clone, Copy)]
pub struct Clip;

impl PeerProtocol for Clip {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let (agent, peer, closed) = (agent.clone(), peer.clone(), conn.clone());
            tokio::spawn(until_closed(closed, async move {
                let reply = match read_frame::<_, Request>(&mut recv).await {
                    Ok(Some(req)) => handle(&agent, &peer, req).await,
                    Ok(None) => return,
                    Err(e) => Response::Error(Error::new(ErrorCode::Invalid, e.to_string())),
                };
                let _ = write_frame(&mut send, &reply).await;
                let _ = send.finish();
            }));
        }
    }
}

async fn handle(agent: &Arc<Agent>, peer: &Peer, req: Request) -> Response {
    if let Err(e) = agent.require(peer, req.min_permission(), describe(&req)) {
        return Response::Error(e);
    }
    // Info still answers, so the phone can say how to turn it on.
    if !super::enabled(agent) && !matches!(req, Request::Info) {
        return Response::Error(Error::new(
            ErrorCode::Forbidden,
            "The clipboard is off on this machine. Turn it on there with: pgpu clip on",
        ));
    }
    match req {
        Request::Info => Response::Info(super::info(agent).await),
        Request::Put { text, secret, once } => {
            if let Err(e) = super::check(&text) {
                return Response::Error(e);
            }
            let place = super::put(agent, text, secret, once).await;
            record(agent, peer, Event::ClipPut, place, "ok");
            Response::Ok
        }
        Request::Get => match super::get(agent).await {
            Ok(Found { place, read, age }) => {
                let (result, reply) = match read {
                    Read::Text(text) => (
                        "ok",
                        Response::Text {
                            text,
                            age_ms: age.map(|a| a.as_millis() as u64),
                        },
                    ),
                    Read::Empty => ("empty", Response::Empty),
                    Read::Secret => ("secret", Response::Secret),
                };
                record(agent, peer, Event::ClipGet, place, result);
                reply
            }
            Err(e) => {
                record(agent, peer, Event::ClipGet, super::place(), &e.message);
                Response::Error(e)
            }
        },
    }
}

/// The target is where the text went or came from, never the text.
fn record(agent: &Agent, peer: &Peer, event: Event, place: Place, result: &str) {
    let target = match place {
        Place::System => "system",
        Place::Memory => "memory",
    };
    if let Err(e) = audit::record(&agent.db(), &peer.key.to_string(), event, target, result) {
        tracing::warn!("could not write audit entry: {e}");
    }
}

/// What a refused request was, for the audit log: never the text.
fn describe(req: &Request) -> &'static str {
    match req {
        Request::Info => "Clipboard Info",
        Request::Put { .. } => "Clipboard Put",
        Request::Get => "Clipboard Get",
    }
}
