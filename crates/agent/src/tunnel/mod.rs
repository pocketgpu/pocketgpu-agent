//! `pgpu/tunnel/1`: forwards allowed requests to Ollama or another model server on this
//! machine (F17, F18).
//!
//! Each stream carries one request. The path must be on the allowlist in
//! `pocketgpu_proto::tunnel::permission_for` (Ollama) or `server_permission_for` (the others,
//! on their known or added ports only: ai/mod.rs) and the phone needs the permission it names.
//! Pulls and deletes are audited with the model name; prompts and replies never are.

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, SendStream};
use pocketgpu_proto::tunnel::{Method, ToAgent, ToPhone, permission_for, server_permission_for};
use pocketgpu_proto::{Error, ErrorCode, Permission, read_frame, write_frame};

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
            // Only ever this machine's own servers: no proxy, and no redirect elsewhere.
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
    agent: &Arc<Agent>,
    peer: &Peer,
    http: &reqwest::Client,
    request: ToAgent,
    send: &mut SendStream,
) -> Result<(), Error> {
    match request {
        ToAgent::Request { method, path, body } => {
            let call = Call {
                method,
                path: &path,
                body,
            };
            ollama(agent, peer, http, call, send).await
        }
        ToAgent::Server {
            port,
            method,
            path,
            body,
        } => {
            let needed =
                server_permission_for(method, &path).ok_or_else(|| refused(method, &path))?;
            let name = crate::ai::name(agent, port);
            agent.require(peer, needed, &format!("{name} {path}"))?;
            if !crate::ai::allowed(agent, port) {
                return Err(Error::new(
                    ErrorCode::Invalid,
                    format!(
                        "PocketGPU doesn't talk to port {port}; add it at the computer with: pgpu ai add {port}"
                    ),
                ));
            }
            let base = format!("http://127.0.0.1:{port}");
            let call = Call {
                method,
                path: &path,
                body,
            };
            pipe(http, &base, call, send, &name).await
        }
        ToAgent::Servers | ToAgent::SetUpOllama { .. } => {
            let json = match request {
                ToAgent::SetUpOllama { start } => {
                    let needed = if start {
                        Permission::Shell
                    } else {
                        Permission::View
                    };
                    agent.require(peer, needed, "set up Ollama")?;
                    let setup = if start {
                        crate::ai::setup::start(agent, &peer.key.to_string())
                    } else {
                        agent.ollama_setup.get()
                    };
                    serde_json::to_vec(&setup)
                }
                _ => {
                    agent.require(peer, Permission::View, "model servers")?;
                    serde_json::to_vec(&crate::ai::servers(agent).await)
                }
            }
            .unwrap_or_default();
            let gone = |_| Error::new(ErrorCode::Unknown, "the phone closed the request");
            write_frame(send, &ToPhone::Head { status: 200 })
                .await
                .map_err(gone)?;
            write_frame(send, &ToPhone::Chunk(json))
                .await
                .map_err(gone)?;
            write_frame(send, &ToPhone::End).await.map_err(gone)
        }
    }
}

/// What the phone asked a server for.
struct Call<'a> {
    method: Method,
    path: &'a str,
    body: Vec<u8>,
}

fn refused(method: Method, path: &str) -> Error {
    Error::new(
        ErrorCode::Invalid,
        format!("PocketGPU doesn't forward {method:?} {path}"),
    )
}

async fn ollama(
    agent: &Agent,
    peer: &Peer,
    http: &reqwest::Client,
    call: Call<'_>,
    send: &mut SendStream,
) -> Result<(), Error> {
    let (method, path) = (call.method, call.path);
    let needed = permission_for(method, path).ok_or_else(|| refused(method, path))?;
    agent.require(peer, needed, &format!("Ollama {path}"))?;
    let audited = match (method, path) {
        (Method::Post, "/api/pull") => Some(Audit::ModelPull),
        (Method::Delete, "/api/delete") => Some(Audit::ModelDelete),
        _ => None,
    };
    if let Some(event) = audited {
        let model = model_name(&call.body);
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
    pipe(http, &base, call, send, "Ollama").await
}

/// Sends the request to the server at `base` and streams its answer back to the phone.
async fn pipe(
    http: &reqwest::Client,
    base: &str,
    call: Call<'_>,
    send: &mut SendStream,
    name: &str,
) -> Result<(), Error> {
    let Call { method, path, body } = call;
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
            format!("{name} isn't running on this machine (nothing answers at {base})")
        } else {
            format!("could not reach {name}: {e}")
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
                    // A closed stream ends here, which drops the request to the server.
                    write_frame(send, &ToPhone::Chunk(part.to_vec()))
                        .await
                        .map_err(gone)?;
                }
            }
            Ok(None) => break,
            Err(e) => {
                return Err(Error::new(
                    ErrorCode::Internal,
                    format!("{name} stopped answering: {e}"),
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
