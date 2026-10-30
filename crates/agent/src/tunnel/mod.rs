//! `pgpu/tunnel/1`: forwards allowed requests to Ollama on this machine (F17, F18).
//!
//! Each stream carries one request. The path must be on the allowlist in
//! `pocketgpu_proto::tunnel::permission_for` and the phone needs the permission it names.
//! Pulls and deletes are audited with the model name; prompts and replies never are.

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, SendStream};
use pocketgpu_proto::tunnel::{Method, ToAgent, ToPhone, permission_for};
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};

use crate::audit::{self, Event as Audit};
use crate::daemon::auth::{Peer, PeerProtocol};
use crate::daemon::state::Agent;

/// Bytes per `Chunk`, well under the frame limit.
const CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Tunnel;

impl PeerProtocol for Tunnel {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            // Only ever this machine's own Ollama: no proxy, and no redirect elsewhere.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        loop {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let (agent, peer, http) = (agent.clone(), peer.clone(), http.clone());
            tokio::spawn(async move {
                let Ok(Some(request)) = read_frame::<_, ToAgent>(&mut recv).await else {
                    return;
                };
                if let Err(e) = forward(&agent, &peer, &http, request, &mut send).await {
                    let _ = write_frame(&mut send, &ToPhone::Error(e)).await;
                }
                let _ = send.finish();
            });
        }
    }
}

async fn forward(
    agent: &Agent,
    peer: &Peer,
    http: &reqwest::Client,
    request: ToAgent,
    send: &mut SendStream,
) -> Result<(), Error> {
    let ToAgent::Request { method, path, body } = request;
    let path = path.as_str();
    let needed = permission_for(method, path).ok_or_else(|| {
        Error::new(
            ErrorCode::Invalid,
            format!("PocketGPU doesn't forward {method:?} {path}"),
        )
    })?;
    agent.require(peer, needed, &format!("Ollama {path}"))?;
    let audited = match (method, path) {
        (Method::Post, "/api/pull") => Some(Audit::ModelPull),
        (Method::Delete, "/api/delete") => Some(Audit::ModelDelete),
        _ => None,
    };
    if let Some(event) = audited {
        let model = model_name(&body);
        let _ = audit::record(
            &agent.db(),
            &peer.key.to_string(),
            event,
            &model,
            "requested",
        );
    }

    let base = agent
        .config
        .read()
        .expect("config lock poisoned")
        .ollama_addr
        .clone();
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let req = match method {
        Method::Get => http.get(&url),
        Method::Post => http.post(&url),
        Method::Delete => http.delete(&url),
    };
    let req = if body.is_empty() {
        req
    } else {
        req.header("content-type", "application/json").body(body)
    };
    let mut res = req.send().await.map_err(|e| {
        let msg = if e.is_connect() || e.is_timeout() {
            format!("Ollama isn't running on this machine (nothing answers at {base})")
        } else {
            format!("could not reach Ollama: {e}")
        };
        Error::new(ErrorCode::NotFound, msg)
    })?;

    let gone = |_| Error::new(ErrorCode::Unknown, "the phone closed the request");
    write_frame(
        send,
        &ToPhone::Head {
            status: res.status().as_u16(),
        },
    )
    .await
    .map_err(gone)?;
    loop {
        match res.chunk().await {
            Ok(Some(bytes)) => {
                for part in bytes.chunks(CHUNK) {
                    // A closed stream ends here, which drops the request to Ollama.
                    write_frame(send, &ToPhone::Chunk(part.to_vec()))
                        .await
                        .map_err(gone)?;
                }
            }
            Ok(None) => break,
            Err(e) => {
                return Err(Error::new(
                    ErrorCode::Internal,
                    format!("Ollama stopped answering: {e}"),
                ));
            }
        }
    }
    write_frame(send, &ToPhone::End).await.map_err(gone)?;
    Ok(())
}

/// The `model` (or older `name`) in a request body, for the audit log.
fn model_name(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("model")
                .or_else(|| v.get("name"))
                .and_then(|m| m.as_str())
                .map(|m| m.chars().take(100).collect())
        })
        .unwrap_or_else(|| "?".into())
}
