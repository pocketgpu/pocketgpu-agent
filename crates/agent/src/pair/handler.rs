//! `pgpu/pair/1`: the only protocol open to unknown keys, and only while a code or an invite
//! (F44) is active. A proof of an invite pairs at once, with the invite's level and end time;
//! a device paired for good that proves one is let in as it is, and the invite stays open.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::PublicKey;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use pocketgpu_proto::msg::pair::{PairRequest, Response};
use pocketgpu_proto::{Error, ErrorCode, Permission, read_frame, write_frame};
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::code::{Ask, Decision, NOBODY_THERE};
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
        if !self.agent.pairing.is_open() && !self.agent.pairing.invites.any_open() {
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

    // A device paired for good keeps what it has: an invite would make it a guest, maybe with
    // less, that is removed when the time is up. It hears it's in with its own level, so an
    // app that lost its computers (reinstalled, its key kept) saves this one again; nothing
    // changes here, and the invite stays for whoever it was meant for.
    if let Some(permission) = super::guest::paired_for_good(agent, &device)
        && agent
            .pairing
            .invites
            .matches(local, &device, &req.secret_proof)
    {
        tracing::info!(remote = %device.fmt_short(), "a paired device used an invite: kept");
        return approve(agent, conn, &mut send, permission).await;
    }
    if let Some(grant) = agent
        .pairing
        .invites
        .take(local, &device, &req.secret_proof)
    {
        if let Err(why) = super::guest::may_join(agent, &grant) {
            return refuse(agent, conn, &mut send, &device, why).await;
        }
        return match super::guest::join(agent, device, &req, &grant) {
            Ok(()) => approve(agent, conn, &mut send, grant.permission).await,
            Err(e) => {
                let reply = Response::Error(Error::new(ErrorCode::Internal, e.to_string()));
                finish(conn, &mut send, &reply).await
            }
        };
    }

    let asks = match agent.pairing.check(local, &device, &req.secret_proof) {
        Ok(asks) => asks,
        Err(refusal) => {
            agent.pairing.invites.missed(&device);
            return refuse(agent, conn, &mut send, &device, &refusal.to_string()).await;
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
        from_lan: false,
    };
    let reply = if asks.send(ask).await.is_err() {
        Response::Error(Error::new(
            ErrorCode::Busy,
            "pairing was cancelled on the computer",
        ))
    } else {
        // Ends when the person decides, or when `pgpu pair` stops (no answer, not a "no").
        // A phone that leaves first (its person said the keys differ, or cancelled) drops
        // the question: `pgpu pair` then stops asking and saves nothing.
        let decision = tokio::select! {
            d = decided => d.unwrap_or(Decision::NoAnswer(NOBODY_THERE)),
            _ = conn.closed() => return Ok(()),
        };
        decision.reply(|| relays(agent))
    };
    finish(conn, &mut send, &reply).await
}

fn relays(agent: &Agent) -> Vec<String> {
    agent
        .config
        .read()
        .expect("config lock poisoned")
        .relays
        .clone()
}

/// Lets a device in at once with `permission`: `Pending` (this computer), then `Approved`.
async fn approve(
    agent: &Agent,
    conn: &Connection,
    send: &mut iroh::endpoint::SendStream,
    permission: Permission,
) -> Result<()> {
    write_frame(
        send,
        &Response::Pending {
            agent_id: *agent.identity.agent_id().as_bytes(),
            agent_name: agent.name(),
        },
    )
    .await?;
    let reply = Response::Approved {
        permission,
        relays: relays(agent),
    };
    finish(conn, send, &reply).await
}

/// Refuses a phone's proof, audited, with words the phone shows.
async fn refuse(
    agent: &Agent,
    conn: &Connection,
    send: &mut iroh::endpoint::SendStream,
    device: &PublicKey,
    why: &str,
) -> Result<()> {
    if let Err(e) = audit::record(
        &agent.db(),
        &device.to_string(),
        Event::Refused,
        "pair",
        why,
    ) {
        tracing::warn!("could not write audit entry: {e}");
    }
    let reply = Response::Error(Error::new(ErrorCode::Forbidden, why));
    finish(conn, send, &reply).await
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
