//! The iroh endpoint phones dial, and the router that hands each protocol to its handler.

use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey};
use pocketgpu_proto::alpn;
use pocketgpu_proto::pairing::PairCode;

use super::auth::Paired;
use super::control::Control;
use super::metrics::Metrics;
use super::state::Agent;
use crate::pair::handler::PairHandler;

/// How the endpoint finds relays.
#[derive(Debug, Clone)]
pub enum Relays {
    /// The URLs in `config.toml`, else our relays (releases) or n0's public ones (development).
    FromConfig,
    /// No relays and no address publishing: tests on one machine.
    LocalOnly,
}

pub async fn bind(agent: &Arc<Agent>, relays: Relays) -> Result<Endpoint> {
    agent.pairing.set_relays(relays.clone());
    bind_key(agent, agent.identity.secret.clone(), relays).await
}

async fn bind_key(agent: &Arc<Agent>, secret: SecretKey, relays: Relays) -> Result<Endpoint> {
    let endpoint = match relays {
        Relays::LocalOnly => {
            Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .transport_config(transport())
                .secret_key(secret)
                .bind()
                .await
        }
        Relays::FromConfig => {
            let mut urls = agent
                .config
                .read()
                .expect("config lock poisoned")
                .relays
                .clone();
            // `pgpu relay set` wins; then our relays when the release names them.
            if urls.is_empty() {
                urls = pocketgpu_proto::default_relays()
                    .into_iter()
                    .map(String::from)
                    .collect();
            }
            // HTTPS_PROXY and friends: relays still work behind a company proxy.
            let mut builder = Endpoint::builder(presets::N0)
                .transport_config(transport())
                .secret_key(secret)
                .proxy_from_env();
            if !urls.is_empty() {
                let parsed = urls
                    .iter()
                    .map(|u| {
                        u.parse::<RelayUrl>()
                            .with_context(|| format!("invalid relay URL {u}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                builder = builder.relay_mode(RelayMode::custom(parsed));
            }
            builder.bind().await
        }
    };
    endpoint.context("could not start the network endpoint")
}

/// Streams one phone may have open at once per connection (QUIC's default is 100); with
/// connections capped per phone too, one phone can't tie up the agent's memory.
const MAX_STREAMS: u32 = 16;

fn transport() -> iroh::endpoint::QuicTransportConfig {
    iroh::endpoint::QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(iroh::endpoint::VarInt::from_u32(MAX_STREAMS))
        .build()
}

/// Serves every protocol until the router is shut down.
pub fn serve(agent: Arc<Agent>, endpoint: Endpoint) -> Router {
    Router::builder(endpoint)
        .accept(
            alpn::PAIR,
            PairHandler::new(agent.clone(), agent.identity.agent_id()),
        )
        .accept(alpn::CONTROL, Paired::new(agent.clone(), Control))
        .accept(alpn::METRICS, Paired::new(agent.clone(), Metrics))
        .accept(
            alpn::JOBS,
            Paired::new(agent.clone(), crate::jobs::handler::Jobs),
        )
        .accept(
            alpn::SHELL,
            Paired::new(agent.clone(), crate::shell::handler::Shell),
        )
        .accept(
            alpn::TUNNEL,
            Paired::new(agent.clone(), crate::tunnel::Tunnel),
        )
        .accept(
            alpn::EVENTS,
            Paired::new(agent.clone(), super::events::Events),
        )
        .accept(
            alpn::DOCKER,
            Paired::new(agent, crate::docker::handler::Docker),
        )
        .spawn()
}

/// The endpoint a phone with only the typed code dials: its key is derived from the code and
/// it serves nothing but pairing. It lives as long as the code.
pub async fn rendezvous(agent: &Arc<Agent>, code: &PairCode) -> Result<Router> {
    let secret = SecretKey::from_bytes(&code.rendezvous_seed());
    let local = secret.public();
    let endpoint = bind_key(agent, secret, agent.pairing.relays()).await?;
    Ok(Router::builder(endpoint)
        .accept(alpn::PAIR, PairHandler::new(agent.clone(), local))
        .spawn())
}
