//! `pgpu/metrics/1`: after `Subscribe`, a sample at most once per interval until the phone closes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use pocketgpu_proto::msg::metrics::{Event, Subscribe};
use pocketgpu_proto::{Permission, read_frame, write_frame};

use super::auth::{Peer, PeerProtocol};
use super::state::Agent;

#[derive(Debug, Clone, Copy)]
pub struct Metrics;

const MIN_INTERVAL: Duration = Duration::from_millis(500);

impl PeerProtocol for Metrics {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let agent = agent.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                let Ok(Some(sub)) = read_frame::<_, Subscribe>(&mut recv).await else {
                    return;
                };
                if agent.require(&peer, Permission::View, "Subscribe").is_err() {
                    return;
                }
                let interval = Duration::from_millis(sub.interval_ms.into()).max(MIN_INTERVAL);
                let mut updates = agent.sampler.subscribe();
                // Send the newest sample straight away so the screen fills instantly.
                if let Some(s) = agent.sampler.latest()
                    && write_frame(&mut send, &Event::Sample(s)).await.is_err()
                {
                    return;
                }
                let mut last = Instant::now();
                while updates.changed().await.is_ok() {
                    if last.elapsed() + Duration::from_millis(50) < interval {
                        continue;
                    }
                    let Some(sample) = updates.borrow_and_update().clone() else {
                        continue;
                    };
                    if write_frame(&mut send, &Event::Sample(sample))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    last = Instant::now();
                }
            });
        }
    }
}
