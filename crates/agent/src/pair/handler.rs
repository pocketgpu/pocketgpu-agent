//! `pgpu/pair/1`: the only protocol open to unknown keys, and only while a code is active.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::PublicKey;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use pocketgpu_proto::msg::pair::{PairRequest, Response};
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::code::Ask;
use crate::audit::{self, Event};
use crate::daemon::state::Agent;

/// How long a phone may take to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves pairing on one endpoint: the agent's own, or the rendezvous endpoint of a code.
#[derive(Debug, Clone)]
pub struct PairHandler {
    agent: Arc<Agent>,
    /// The ID of the endpoint this handler serves; proofs are bound to it.
    local: PublicKey,
}

impl PairHandler {
    pub fn new(agent: Arc<Agent>, local: PublicKey) -> Self {
        Self { agent, local }
    }
}

impl ProtocolHandler for PairHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let device = conn.remote_id();
        if !self.agent.pairing.is_open() {
            tracing::info!(remote = %device.fmt_short(), "refused pairing: no active code");
            conn.close(403u32.into(), b"no pairing code");
            return Ok(());
        }
        if let Err(e) = serve(&self.agent, &self.local, device, &conn).await {
            tracing::debug!(remote = %device.fmt_short(), "pairing ended: {e:#}");
        }
        Ok(())
    }
}

async fn serve(
    agent: &Arc<Agent>,
    local: &PublicKey,
    device: PublicKey,
    conn: &Connection,
) -> Result<()> {
    let (mut send, mut recv) = timeout(REQUEST_TIMEOUT, conn.accept_bi())
        .await
        .context("no request in time")??;
    let req: PairRequest = timeout(REQUEST_TIMEOUT, read_frame(&mut recv))
        .await
        .context("no request in time")??
        .context("stream closed before the request")?;

    let asks = match agent.pairing.check(local, &device, &req.secret_proof) {
        Ok(asks) => asks,
        Err(refusal) => {
            if let Err(e) = audit::record(
                &agent.db(),
                &device.to_string(),
                Event::Refused,
                "pair",
                &refusal.to_string(),
            ) {
                tracing::warn!("could not write audit entry: {e}");
            }
            let reply = Response::Error(Error::new(ErrorCode::Forbidden, refusal.to_string()));
            return finish(conn, &mut send, &reply).await;
        }
    };

    write_frame(
        &mut send,
        &Response::Pending {
            agent_id: *agent.identity.agent_id().as_bytes(),
            agent_name: agent.name(),
        },
    )
    .await?;

    let (decision, decided) = oneshot::channel();
    let ask = Ask {
        device,
        device_name: req.device_name,
        platform: req.platform,
        // Tickets are 128-bit hex from the push server, as `SetPushTicket` checks.
        push_ticket: req
            .push_ticket
            .filter(|t| t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit())),
        decision,
    };
    let reply = if asks.send(ask).await.is_err() {
        Response::Error(Error::new(
            ErrorCode::Busy,
            "pairing was cancelled on the machine",
        ))
    } else {
        // Ends when the person decides, or when `pgpu pair` stops (counted as a denial).
        match decided.await {
            Ok(Some(permission)) => Response::Approved {
                permission,
                relays: agent
                    .config
                    .read()
                    .expect("config lock poisoned")
                    .relays
                    .clone(),
            },
            _ => Response::Denied,
        }
    };
    finish(conn, &mut send, &reply).await
}

/// Sends the last reply and gives the phone a moment to read it before the connection drops.
async fn finish(
    conn: &Connection,
    send: &mut iroh::endpoint::SendStream,
    reply: &Response,
) -> Result<()> {
    write_frame(send, reply).await?;
    send.finish()?;
    let _ = timeout(Duration::from_secs(5), conn.closed()).await;
    Ok(())
}
