//! `pgpu/events/1`: alerts as they fire, to apps that are open (F31 desktop notifications).
//! The same alerts as push and channels, and held the same way while alerts are paused.

use std::sync::Arc;

use iroh::endpoint::Connection;
use pocketgpu_proto::msg::events::Subscribe;
use pocketgpu_proto::{Permission, read_frame, write_frame};
use tokio::sync::broadcast::error::RecvError;

use super::auth::{Peer, PeerProtocol};
use super::state::Agent;

#[derive(Debug, Clone, Copy)]
pub struct Events;

impl PeerProtocol for Events {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let (agent, peer, closed) = (agent.clone(), peer.clone(), conn.clone());
            tokio::spawn(super::auth::until_closed(closed, async move {
                let Ok(Some(Subscribe)) = read_frame::<_, Subscribe>(&mut recv).await else {
                    return;
                };
                if agent.require(&peer, Permission::View, "Events").is_err() {
                    return;
                }
                let mut alerts = agent.events.subscribe();
                loop {
                    let alert = match alerts.recv().await {
                        Ok(a) => a,
                        // A slow app misses some; it keeps the newest.
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return,
                    };
                    // AI assistant notes and alerts are for the owner's devices only.
                    if crate::alerts::assistant::owner_only(&alert.kind) && is_guest(&agent, &peer)
                    {
                        continue;
                    }
                    // Held actions and their settings: Full control devices only (F32).
                    if crate::alerts::assistant::shell_only(&alert.kind)
                        && agent.permission_of(&peer.key) != Some(Permission::Shell)
                    {
                        continue;
                    }
                    if write_frame(&mut send, &alert).await.is_err() {
                        return;
                    }
                }
            }));
        }
    }
}

/// Checked per alert, not once: a guest's access can change while its app stays open.
fn is_guest(agent: &Agent, peer: &Peer) -> bool {
    // A database error counts as a guest: an owner-only alert is better missed than leaked.
    crate::store::devices::is_guest(&agent.db(), &peer.key.to_string()).unwrap_or(true)
}
