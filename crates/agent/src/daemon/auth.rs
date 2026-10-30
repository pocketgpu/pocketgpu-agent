//! Who may connect and what each phone may do.
//!
//! Unknown keys are dropped before a single message is read. Every request is then checked
//! against the phone's permission, and refusals are written to the audit log.

use std::sync::Arc;

use iroh::PublicKey;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use pocketgpu_proto::{Error, Permission};
use rusqlite::OptionalExtension;

use super::state::Agent;
use crate::audit::{self, Event};

/// A paired phone on an open connection. Its permission is looked up on every request.
#[derive(Debug, Clone)]
pub struct Peer {
    pub key: PublicKey,
}

impl Agent {
    /// The phone's permission, or `None` if it is not paired.
    pub fn permission_of(&self, key: &PublicKey) -> Option<Permission> {
        let db = self.db();
        let perm: Option<String> = db
            .query_row(
                "SELECT permission FROM devices WHERE device_id = ?1",
                [key.to_string()],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        perm.and_then(|p| p.parse().ok())
    }

    /// Checks a request against the phone's permission as it is now (it may have changed or
    /// been revoked since the phone connected); refusals are audited.
    pub fn require(&self, peer: &Peer, needed: Permission, what: &str) -> Result<(), Error> {
        let err = match self.permission_of(&peer.key) {
            Some(have) if have >= needed => return Ok(()),
            Some(_) => Error::forbidden(needed),
            None => Error::new(
                pocketgpu_proto::ErrorCode::Forbidden,
                "this phone is no longer paired with this machine",
            ),
        };
        if !audit_refusal(&peer.key) {
            return Err(err);
        }
        let db = self.db();
        if let Err(e) = audit::record(
            &db,
            &peer.key.to_string(),
            Event::Refused,
            what,
            &err.message,
        ) {
            tracing::warn!("could not write audit entry: {e}");
        }
        Err(err)
    }
}

/// Refusals written to the audit log per phone a minute; a phone retrying in a loop would
/// otherwise fill it.
const REFUSALS_PER_MINUTE: u32 = 30;

/// Whether this refusal goes in the audit log.
fn audit_refusal(key: &PublicKey) -> bool {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};
    static SEEN: LazyLock<Mutex<HashMap<PublicKey, (Instant, u32)>>> =
        LazyLock::new(Mutex::default);
    let mut seen = SEEN.lock().expect("refusal lock poisoned");
    let now = Instant::now();
    if seen.len() > 1000 {
        seen.retain(|_, (since, _)| now.duration_since(*since) < Duration::from_secs(60));
    }
    let (since, count) = seen.entry(*key).or_insert((now, 0));
    if now.duration_since(*since) >= Duration::from_secs(60) {
        (*since, *count) = (now, 0);
    }
    *count += 1;
    *count <= REFUSALS_PER_MINUTE
}

/// Runs a stream's task until it ends or its connection closes (the phone left, or was
/// revoked): long-lived streams such as followed logs would otherwise wait for their next
/// write to notice.
pub async fn until_closed(conn: Connection, task: impl Future<Output = ()>) {
    tokio::select! {
        () = task => {}
        _ = conn.closed() => {}
    }
}

/// A protocol served only to paired phones.
pub trait PeerProtocol: Send + Sync + std::fmt::Debug + 'static {
    fn serve(
        &self,
        agent: Arc<Agent>,
        peer: Peer,
        conn: Connection,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Wraps a protocol so unpaired keys are closed before the protocol sees the connection.
#[derive(Debug, Clone)]
pub struct Paired<P> {
    agent: Arc<Agent>,
    inner: Arc<P>,
}

impl<P> Paired<P> {
    pub fn new(agent: Arc<Agent>, inner: P) -> Self {
        Self {
            agent,
            inner: Arc::new(inner),
        }
    }
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("id", &self.identity.fingerprint())
            .finish()
    }
}

impl<P: PeerProtocol> ProtocolHandler for Paired<P> {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let key = conn.remote_id();
        if self.agent.permission_of(&key).is_none() {
            tracing::debug!(remote = %key.fmt_short(), "refused unpaired device");
            conn.close(403u32.into(), b"not paired");
            return Ok(());
        }
        self.agent.touch(&key);
        self.agent.track(key, conn.clone());
        // Revoked between the check and `track`: `drop_connections` missed this one.
        if self.agent.permission_of(&key).is_none() {
            conn.close(1u32.into(), b"revoked");
            return Ok(());
        }
        let peer = Peer { key };
        if let Err(e) = self.inner.serve(self.agent.clone(), peer, conn).await {
            tracing::debug!(remote = %key.fmt_short(), "connection ended: {e:#}");
        }
        Ok(())
    }
}

impl Agent {
    fn touch(&self, key: &PublicKey) {
        let db = self.db();
        let _ = db.execute(
            "UPDATE devices SET last_seen = ?1 WHERE device_id = ?2",
            rusqlite::params![crate::store::db::now_ms(), key.to_string()],
        );
    }
}
