//! `pgpu/apps/1` (F42): `List` gets one reply; `Open` is checked (Shell, apps on, the port
//! allowed), connects to the app on this machine's `localhost`, answers `Opened` and then
//! copies bytes both ways until either side ends. Permissions are checked on every stream,
//! and again every second while one is open: a device that loses Shell (or apps turned off,
//! or that app hidden) loses its open connections too, such as a Jupyter kernel's.

use std::sync::Arc;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use pocketgpu_proto::apps::{Request, Response};
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::daemon::auth::{Peer, PeerProtocol, until_closed};
use crate::daemon::state::Agent;

#[derive(Debug, Clone, Copy)]
pub struct Apps;

impl PeerProtocol for Apps {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (send, mut recv) = conn.accept_bi().await?;
            let (agent, peer, closed) = (agent.clone(), peer.clone(), conn.clone());
            tokio::spawn(until_closed(closed, async move {
                match read_frame::<_, Request>(&mut recv).await {
                    Ok(Some(req)) => handle(&agent, &peer, req, send, recv).await,
                    Ok(None) => {}
                    Err(e) => {
                        let err = Error::new(ErrorCode::Invalid, e.to_string());
                        reply(send, Response::Error(err)).await;
                    }
                }
            }));
        }
    }
}

async fn reply(mut send: SendStream, r: Response) {
    let _ = write_frame(&mut send, &r).await;
    let _ = send.finish();
}

async fn handle(agent: &Arc<Agent>, peer: &Peer, req: Request, send: SendStream, recv: RecvStream) {
    let what = match req {
        Request::List => "Apps List",
        Request::Open { .. } => "Apps Open",
        Request::Start { .. } => "Apps Start",
    };
    if let Err(e) = agent.require(peer, req.min_permission(), what) {
        return reply(send, Response::Error(e)).await;
    }
    match req {
        Request::List => reply(send, Response::Apps(super::list(agent).await)).await,
        Request::Start { port } => {
            let r = match super::check(agent, port) {
                Ok(()) => Response::Start {
                    path: super::jupyter::start_path(port).unwrap_or_else(|| "/".into()),
                },
                Err(e) => Response::Error(e),
            };
            reply(send, r).await
        }
        Request::Open { port } => {
            if let Err(e) = super::check(agent, port) {
                return reply(send, Response::Error(e)).await;
            }
            let tcp = match super::find::connect(port).await {
                Ok(t) => t,
                Err(_) => {
                    let e = Error::new(
                        ErrorCode::NotFound,
                        format!(
                            "Nothing answers on port {port} on this machine now. Is the app running?"
                        ),
                    );
                    return reply(send, Response::Error(e)).await;
                }
            };
            super::record_open(agent, peer.key.to_string(), port);
            tokio::select! {
                () = pipe(send, recv, tcp) => {}
                () = still_allowed(agent, peer, port) => {}
            }
        }
    }
}

/// Returns once the device may no longer use this app: checked every second.
async fn still_allowed(agent: &Agent, peer: &Peer, port: u16) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.tick().await;
    loop {
        tick.tick().await;
        let shell = agent
            .permission_of(&peer.key)
            .is_some_and(|p| p >= pocketgpu_proto::Permission::Shell);
        if !shell || super::check(agent, port).is_err() {
            return;
        }
    }
}

/// `Opened`, then bytes both ways; each side's end is passed on to the other.
async fn pipe(mut send: SendStream, mut recv: RecvStream, tcp: TcpStream) {
    if write_frame(&mut send, &Response::Opened).await.is_err() {
        return;
    }
    let (mut from_app, mut to_app) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut recv, &mut to_app).await;
        let _ = to_app.shutdown().await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut from_app, &mut send).await;
        let _ = send.finish();
    };
    tokio::join!(up, down);
}
