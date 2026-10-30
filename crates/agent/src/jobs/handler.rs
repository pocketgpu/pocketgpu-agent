//! `pgpu/jobs/1`: one request per bidirectional stream; `FollowLog` streams until the job
//! ends and its log is fully sent.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, SendStream};
use pocketgpu_proto::msg::jobs::{Request, Response};
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};

use super::{StopError, store};
use crate::audit::{self, Event};
use crate::daemon::auth::{Peer, PeerProtocol};
use crate::daemon::state::Agent;

/// Largest log chunk per frame.
const CHUNK: usize = 32 * 1024;
const POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy)]
pub struct Jobs;

impl PeerProtocol for Jobs {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let (agent, peer, closed) = (agent.clone(), peer.clone(), conn.clone());
            tokio::spawn(crate::daemon::auth::until_closed(closed, async move {
                let req = match read_frame::<_, Request>(&mut recv).await {
                    Ok(Some(req)) => req,
                    Ok(None) => return,
                    Err(e) => {
                        let err = Error::new(ErrorCode::Invalid, e.to_string());
                        let _ = write_frame(&mut send, &Response::Error(err)).await;
                        return;
                    }
                };
                if let Err(e) = handle(&agent, &peer, req, &mut send).await {
                    tracing::debug!("jobs stream ended: {e:#}");
                }
                let _ = send.finish();
            }));
        }
    }
}

async fn handle(
    agent: &Arc<Agent>,
    peer: &Peer,
    req: Request,
    send: &mut SendStream,
) -> anyhow::Result<()> {
    if let Err(e) = agent.require(peer, req.min_permission(), &describe(&req)) {
        return Ok(write_frame(send, &Response::Error(e)).await?);
    }
    let reply = match req {
        Request::ListJobs { limit, before } => {
            let jobs = store::list(&agent.db(), limit, before);
            Response::Jobs(jobs.unwrap_or_default())
        }
        Request::GetJob { id } => {
            let job = store::get(&agent.db(), id);
            match job {
                Ok(Some((detail, _))) => Response::Job(detail),
                _ => not_found(id),
            }
        }
        Request::FollowLog { id, from_offset } => {
            // Bind first: the database lock must not be held across the stream.
            let job = store::get(&agent.db(), id);
            let log = match job {
                Ok(Some((_, log))) => log,
                _ => return Ok(write_frame(send, &not_found(id)).await?),
            };
            return follow(agent, id, &log, from_offset, send).await;
        }
        Request::StopJob { id } => {
            let result = agent.jobs.stop(agent, id);
            let outcome = match &result {
                Ok(()) => "ok".to_string(),
                Err(e) => e.to_string(),
            };
            if let Err(e) = audit::record(
                &agent.db(),
                &peer.key.to_string(),
                Event::JobStop,
                &format!("job {id}"),
                &outcome,
            ) {
                tracing::warn!("could not write audit entry: {e}");
            }
            match result {
                Ok(()) => Response::Ok,
                Err(StopError::NotFound) => not_found(id),
                Err(e @ StopError::NotRunning) => {
                    Response::Error(Error::new(ErrorCode::Invalid, e.to_string()))
                }
            }
        }
        Request::SetJobAlerts { id, on } => {
            let set = store::set_alerts(&agent.db(), id, on);
            match set {
                Ok(true) => Response::Ok,
                _ => not_found(id),
            }
        }
    };
    Ok(write_frame(send, &reply).await?)
}

/// Sends the log from `offset`, then new output as it's written, then `End`.
async fn follow(
    agent: &Arc<Agent>,
    id: u64,
    log: &Path,
    mut offset: u64,
    send: &mut SendStream,
) -> anyhow::Result<()> {
    loop {
        // Check before reading, so output written just before the end is still sent.
        let running = agent.jobs.is_running(id);
        let chunk = read_from(log, &mut offset).await;
        if !chunk.is_empty() {
            let at = offset - chunk.len() as u64;
            write_frame(
                send,
                &Response::LogChunk {
                    offset: at,
                    bytes: chunk,
                },
            )
            .await?;
            continue;
        }
        if !running {
            let exit_code = store::get(&agent.db(), id)
                .ok()
                .flatten()
                .and_then(|(d, _)| d.summary.exit_code);
            return Ok(write_frame(send, &Response::End { exit_code }).await?);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Up to [`CHUNK`] bytes from `offset`, advancing it. A log cut down to its end restarts
/// from its new end.
async fn read_from(path: &Path, offset: &mut u64) -> Vec<u8> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let Ok(mut f) = tokio::fs::File::open(path).await else {
        return Vec::new();
    };
    let len = f.metadata().await.map_or(0, |m| m.len());
    if len < *offset {
        *offset = len;
    }
    if f.seek(std::io::SeekFrom::Start(*offset)).await.is_err() {
        return Vec::new();
    }
    let mut buf = vec![0u8; CHUNK];
    let n = f.read(&mut buf).await.unwrap_or(0);
    buf.truncate(n);
    *offset += n as u64;
    buf
}

fn not_found(id: u64) -> Response {
    Response::Error(Error::new(ErrorCode::NotFound, format!("no job {id}")))
}

/// Short text for the audit log.
fn describe(req: &Request) -> String {
    match req {
        Request::ListJobs { .. } => "ListJobs".into(),
        Request::GetJob { id } => format!("GetJob {id}"),
        Request::FollowLog { id, .. } => format!("FollowLog {id}"),
        Request::StopJob { id } => format!("StopJob {id}"),
        Request::SetJobAlerts { id, .. } => format!("SetJobAlerts {id}"),
    }
}
