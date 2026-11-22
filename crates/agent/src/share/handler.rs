//! `pgpu/model-guest/1`: the only protocol a guest key may use (F48).
//!
//! Not behind `Paired`: unknown keys may send exactly one thing, a claim with a proof of a
//! waiting share's secret. A claimed key may then ask for its status and chat with its
//! share's model, and nothing else; its row is read again for every request, so a stopped
//! or ended share refuses at once. Paired devices are never guests.

use std::sync::Arc;
use std::time::Duration;

use iroh::PublicKey;
use iroh::endpoint::{Connection, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use pocketgpu_proto::pairing::{proof, proof_matches};
use pocketgpu_proto::share::{AgentToGuest, GuestRefusal, GuestToAgent, ShareState};
use pocketgpu_proto::{read_frame, write_frame};

use super::{chat, end, guest_status, is_over, store, target};
use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

#[derive(Debug, Clone)]
pub struct ModelGuest {
    agent: Arc<Agent>,
}

impl ModelGuest {
    pub fn new(agent: Arc<Agent>) -> Self {
        Self { agent }
    }
}

impl ProtocolHandler for ModelGuest {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let key = conn.remote_id();
        // A paired device keeps its own way in; it is never also a guest.
        if self.agent.permission_of(&key).is_some() {
            conn.close(403u32.into(), b"not a guest");
            return Ok(());
        }
        loop {
            let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                return Ok(());
            };
            let Ok(Some(req)) = read_frame::<_, GuestToAgent>(&mut recv).await else {
                return Ok(());
            };
            let (keep, send) = match req {
                GuestToAgent::Claim { share, proof } => {
                    let (reply, fresh) = claim(&self.agent, key, share, proof);
                    let ok = matches!(reply, AgentToGuest::Claimed(_));
                    if ok {
                        self.agent.track(key, conn.clone());
                    }
                    if let Some(share) = fresh {
                        let agent = self.agent.clone();
                        tokio::spawn(async move {
                            match crate::alerts::guest_started::send(&agent, &share).await {
                                Ok(n) => tracing::info!(phones = n, "guest-started push sent"),
                                Err(e) => tracing::warn!("guest-started push not sent: {e:#}"),
                            }
                        });
                    }
                    let _ = write_frame(&mut send, &reply).await;
                    let _ = send.finish();
                    let keep =
                        ok || !matches!(reply, AgentToGuest::Refused(GuestRefusal::TooManyTries));
                    (keep, Some(send))
                }
                GuestToAgent::Peek { share, proof } => {
                    let reply = peek(&self.agent, key, share, proof);
                    let ok = !matches!(reply, AgentToGuest::Refused(GuestRefusal::TooManyTries));
                    let _ = write_frame(&mut send, &reply).await;
                    let _ = send.finish();
                    (ok, Some(send))
                }
                GuestToAgent::Status => {
                    let keep = serve(&self.agent, &conn, key, None, &mut send).await;
                    (keep, Some(send))
                }
                GuestToAgent::Chat { messages } => {
                    // One answer per stream; the next request may come on another stream.
                    let (agent, conn2) = (self.agent.clone(), conn.clone());
                    tokio::spawn(async move {
                        serve(&agent, &conn2, key, Some(messages), &mut send).await;
                    });
                    (true, None)
                }
            };
            if !keep {
                // The refusal ("this link has ended") must reach the browser before the close
                // does, or it only sees a dropped connection and says the computer is off.
                if let Some(send) = send {
                    let _ = tokio::time::timeout(Duration::from_secs(3), send.stopped()).await;
                }
                conn.close(403u32.into(), b"not a guest");
                return Ok(());
            }
        }
    }
}

/// Status or chat for a claimed key; false (and the connection closes) for any other key.
async fn serve(
    agent: &Arc<Agent>,
    conn: &Connection,
    key: PublicKey,
    chat_messages: Option<Vec<pocketgpu_proto::share::ChatMsg>>,
    send: &mut SendStream,
) -> bool {
    let now = now_ms();
    // Two statements: one lock of the database each (a lock kept for the whole chain
    // would wait on itself).
    let id = store::share_of_guest(&agent.db(), &key.to_string())
        .ok()
        .flatten();
    let share = id.and_then(|id| store::get(&agent.db(), id).ok().flatten());
    let Some(share) = share else {
        let _ = write_frame(send, &AgentToGuest::Refused(GuestRefusal::NotForYou)).await;
        let _ = send.finish();
        return false;
    };
    if is_over(&share, now) {
        if share.state != ShareState::Ended {
            end(agent, share.id, "local", "ended");
        }
        let _ = write_frame(send, &AgentToGuest::Refused(GuestRefusal::Ended)).await;
        let _ = send.finish();
        return false;
    }
    agent.track(key, conn.clone());
    let _ = store::touch_guest(&agent.db(), &key.to_string(), now);
    match chat_messages {
        None => {
            let mut status = guest_status(agent, &share, now);
            if status.state == pocketgpu_proto::share::GuestState::Ready
                && !chat::model_available(agent, share.server, &share.model).await
            {
                status.state = pocketgpu_proto::share::GuestState::ModelUnavailable;
            }
            let _ = write_frame(send, &AgentToGuest::Status(status)).await;
        }
        Some(messages) => chat::run(agent, &share, messages, send).await,
    }
    let _ = send.finish();
    true
}

/// The share's status for a right proof, without claiming it; refusals as for a claim.
pub fn peek(agent: &Agent, key: PublicKey, share_id: u64, given: [u8; 32]) -> AgentToGuest {
    let refused = AgentToGuest::Refused;
    if !agent.shares.may_try(&key) {
        return refused(GuestRefusal::TooManyTries);
    }
    let now = now_ms();
    let Some(share) = store::get(&agent.db(), share_id).ok().flatten() else {
        agent.shares.wrong(key, share_id);
        return refused(GuestRefusal::NotForYou);
    };
    let expected = proof(
        &share.secret,
        agent.identity.agent_id().as_bytes(),
        key.as_bytes(),
    );
    if !proof_matches(&expected, &given) {
        if agent.shares.wrong(key, share_id) && share.state == ShareState::Waiting {
            end(agent, share_id, "local", "too many wrong tries");
        }
        return refused(GuestRefusal::NotForYou);
    }
    let mine = store::share_of_guest(&agent.db(), &key.to_string())
        .ok()
        .flatten()
        == Some(share_id);
    match share.state {
        _ if is_over(&share, now) => refused(GuestRefusal::Ended),
        ShareState::Active if !mine => refused(GuestRefusal::AlreadyClaimed),
        ShareState::Waiting if now >= share.claim_until_ms => refused(GuestRefusal::LinkExpired),
        _ => AgentToGuest::Status(guest_status(agent, &share, now)),
    }
}

/// Checks a claim and, when it is right, makes this key the share's only guest. The share
/// comes back too when this claim is the first (for the owner's push), not when the same
/// browser opens its link again.
pub fn claim(
    agent: &Agent,
    key: PublicKey,
    share_id: u64,
    given: [u8; 32],
) -> (AgentToGuest, Option<store::Share>) {
    let refused = |r| (AgentToGuest::Refused(r), None);
    if !agent.shares.may_try(&key) {
        return refused(GuestRefusal::TooManyTries);
    }
    let now = now_ms();
    let share = store::get(&agent.db(), share_id).ok().flatten();
    let Some(share) = share else {
        agent.shares.wrong(key, share_id);
        return refused(GuestRefusal::NotForYou);
    };
    let expected = proof(
        &share.secret,
        agent.identity.agent_id().as_bytes(),
        key.as_bytes(),
    );
    if !proof_matches(&expected, &given) {
        if agent.shares.wrong(key, share_id) && share.state == ShareState::Waiting {
            end(agent, share_id, "local", "too many wrong tries");
        }
        return refused(GuestRefusal::NotForYou);
    }
    let key_text = key.to_string();
    // The same browser opening its link again: already this share's guest.
    if store::share_of_guest(&agent.db(), &key_text).ok().flatten() == Some(share_id)
        && !is_over(&share, now)
    {
        return (
            AgentToGuest::Claimed(guest_status(agent, &share, now)),
            None,
        );
    }
    match share.state {
        ShareState::Ended => return refused(GuestRefusal::Ended),
        ShareState::Active => return refused(GuestRefusal::AlreadyClaimed),
        ShareState::Waiting if now >= share.claim_until_ms || is_over(&share, now) => {
            return refused(GuestRefusal::LinkExpired);
        }
        ShareState::Waiting => {}
    }
    let db = agent.db();
    match store::insert_guest(&db, &key_text, share_id, now) {
        Ok(true) => {}
        Ok(false) => return refused(GuestRefusal::AlreadyClaimed),
        Err(_) => return refused(GuestRefusal::NotForYou),
    }
    let _ = store::set_state(&db, share_id, ShareState::Active, now);
    let short = key.fmt_short().to_string();
    let _ = audit::record(&db, &short, Event::Share, &target(&share), "claimed");
    drop(db);
    let share = store::get(&agent.db(), share_id)
        .ok()
        .flatten()
        .unwrap_or(share);
    let status = guest_status(agent, &share, now);
    (AgentToGuest::Claimed(status), Some(share))
}
