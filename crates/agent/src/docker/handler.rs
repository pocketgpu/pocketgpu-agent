//! `pgpu/docker/1` (F28): containers for paired phones, off until `pgpu docker on` at the
//! machine. Listing and logs need View; start, stop and restart need Control and are
//! audited. Phones can't create containers or run commands in them.

use std::sync::Arc;

use http_body_util::BodyExt;
use iroh::endpoint::{Connection, SendStream};
use pocketgpu_proto::docker::{LogLine, Request, Response, valid_id};
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};

use super::api::{self, DockerError};
use super::logs::Lines;
use crate::audit::{self, Event};
use crate::daemon::auth::{Peer, PeerProtocol};
use crate::daemon::state::Agent;

/// Lines sent per frame at most, and the most a phone may ask to see from before.
const BATCH: usize = 200;
const MAX_TAIL: u32 = 1000;

#[derive(Debug, Clone, Copy)]
pub struct Docker;

impl PeerProtocol for Docker {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let (agent, peer, closed) = (agent.clone(), peer.clone(), conn.clone());
            tokio::spawn(crate::daemon::auth::until_closed(closed, async move {
                let reply = match read_frame::<_, Request>(&mut recv).await {
                    Ok(Some(req)) => handle(&agent, &peer, req, &mut send).await,
                    Ok(None) => return,
                    Err(e) => Some(Response::Error(Error::new(
                        ErrorCode::Invalid,
                        e.to_string(),
                    ))),
                };
                if let Some(reply) = reply {
                    let _ = write_frame(&mut send, &reply).await;
                }
                let _ = send.finish();
            }));
        }
    }
}

fn enabled(agent: &Agent) -> bool {
    agent.config.read().expect("config lock poisoned").docker
}

pub fn to_error(e: DockerError) -> Error {
    let code = match e {
        DockerError::NoContainer(_) => ErrorCode::NotFound,
        DockerError::NoPermission => ErrorCode::Forbidden,
        DockerError::NotRunning(_) => ErrorCode::Busy,
        DockerError::Docker(_) => ErrorCode::Internal,
    };
    Error::new(code, e.to_string())
}

/// The reply, or `None` when logs were streamed.
async fn handle(
    agent: &Arc<Agent>,
    peer: &Peer,
    req: Request,
    send: &mut SendStream,
) -> Option<Response> {
    if let Err(e) = agent.require(peer, req.min_permission(), &describe(&req)) {
        return Some(Response::Error(e));
    }
    if !enabled(agent) {
        return Some(Response::Error(Error::new(
            ErrorCode::Forbidden,
            "Docker is off on this machine. Turn it on there with: pgpu docker on",
        )));
    }
    if let Request::Act { id, .. } | Request::Logs { id, .. } = &req
        && !valid_id(id)
    {
        return Some(Response::Error(Error::new(
            ErrorCode::Invalid,
            "not a container ID",
        )));
    }
    match req {
        Request::List => Some(match api::list().await {
            Ok(list) => Response::Containers(list),
            Err(e) => Response::Error(to_error(e)),
        }),
        Request::Act { id, action } => {
            let result = api::act(&id, action).await;
            let outcome = match &result {
                Ok(()) => "ok".to_string(),
                Err(e) => e.to_string(),
            };
            let target = format!("{} {id}", action.as_str());
            if let Err(e) = audit::record(
                &agent.db(),
                &peer.key.to_string(),
                Event::Container,
                &target,
                &outcome,
            ) {
                tracing::warn!("could not write audit entry: {e}");
            }
            Some(match result {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(to_error(e)),
            })
        }
        Request::Logs { id, tail } => match follow(agent, &id, tail.min(MAX_TAIL), send).await {
            Ok(()) => None,
            Err(e) => Some(Response::Error(to_error(e))),
        },
    }
}

/// Streams the log until the container stops, the phone closes the stream, or Docker is
/// turned off at the machine.
async fn follow(
    agent: &Agent,
    id: &str,
    tail: u32,
    send: &mut SendStream,
) -> Result<(), DockerError> {
    let mut lines = Lines::new(api::has_tty(id).await?);
    let mut body = api::logs(id, tail).await?;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else { break };
        let Some(data) = frame.data_ref() else {
            continue;
        };
        if !enabled(agent) || send_lines(send, lines.push(data)).await.is_err() {
            return Ok(());
        }
    }
    let _ = send_lines(send, lines.finish()).await;
    Ok(())
}

async fn send_lines(send: &mut SendStream, lines: Vec<LogLine>) -> anyhow::Result<()> {
    for batch in lines.chunks(BATCH) {
        write_frame(send, &Response::Log(batch.to_vec())).await?;
    }
    Ok(())
}

fn describe(req: &Request) -> String {
    match req {
        Request::List => "Docker List".into(),
        Request::Act { id, action } => format!("Docker {} {id}", action.as_str()),
        Request::Logs { id, .. } => format!("Docker Logs {id}"),
    }
}
