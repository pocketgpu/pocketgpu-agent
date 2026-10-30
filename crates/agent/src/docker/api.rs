//! A small Docker Engine API client over the local socket (F28): list containers, start,
//! stop and restart them, and follow their logs. HTTP/1.1 through hyper, which the agent
//! already carries for reqwest.

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use pocketgpu_proto::docker::{Action, Container};
use serde::Deserialize;

/// Why Docker couldn't do something, in words for the phone.
#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("Docker isn't running on this machine ({0})")]
    NotRunning(String),
    #[error(
        "the agent may not use Docker: add your user to the docker group (sudo usermod -aG docker $USER), then log in again"
    )]
    NoPermission,
    #[error("no container {0}")]
    NoContainer(String),
    #[error("Docker said: {0}")]
    Docker(String),
}

#[cfg(unix)]
type Stream = tokio::net::UnixStream;
#[cfg(windows)]
type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Where Docker listens: `DOCKER_HOST` first, then the usual places (the system daemon,
/// Docker Desktop, rootless Docker, Colima).
#[cfg(unix)]
fn sockets() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var("DOCKER_HOST")
        .ok()
        .and_then(|h| h.strip_prefix("unix://").map(std::path::PathBuf::from))
    {
        out.push(p);
    }
    out.push("/var/run/docker.sock".into());
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
        out.push(home.join(".docker/run/docker.sock"));
        out.push(home.join(".colima/default/docker.sock"));
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    out.push(format!("/run/user/{}/docker.sock", unsafe { libc::getuid() }).into());
    out
}

#[cfg(unix)]
async fn connect() -> Result<Stream, DockerError> {
    let mut denied = false;
    for path in sockets() {
        // The socket belongs to root or this user: another local user could otherwise place
        // one where the agent looks (macOS has no Docker in /var/run until Docker Desktop
        // links it there).
        if let Ok(meta) = std::fs::metadata(&path) {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: getuid has no preconditions and cannot fail.
            let me = unsafe { libc::getuid() };
            if meta.uid() != 0 && meta.uid() != me {
                continue;
            }
        }
        match tokio::net::UnixStream::connect(&path).await {
            Ok(s) => return Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied = true,
            Err(_) => {}
        }
    }
    Err(if denied {
        DockerError::NoPermission
    } else {
        DockerError::NotRunning("no Docker socket found".into())
    })
}

#[cfg(windows)]
async fn connect() -> Result<Stream, DockerError> {
    let pipe = std::env::var("DOCKER_HOST")
        .ok()
        .and_then(|h| h.strip_prefix("npipe://").map(|p| p.replace('/', "\\")))
        .unwrap_or_else(|| r"\\.\pipe\docker_engine".into());
    let stream = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(&pipe)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => DockerError::NoPermission,
            _ => DockerError::NotRunning("is Docker Desktop running?".into()),
        })?;
    // Another user's program could have taken the pipe's name first.
    if !pocketgpu_client::local::winpipe::owner_is_trusted(&stream) {
        return Err(DockerError::NotRunning(
            "the Docker pipe belongs to another user; not using it".into(),
        ));
    }
    Ok(stream)
}

/// One request on a fresh connection; the body streams as Docker sends it.
async fn call(method: Method, path: &str) -> Result<Response<Incoming>, DockerError> {
    let io = TokioIo::new(connect().await?);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| DockerError::NotRunning(e.to_string()))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "docker")
        .body(Empty::<Bytes>::new())
        .expect("a valid request");
    sender
        .send_request(req)
        .await
        .map_err(|e| DockerError::NotRunning(e.to_string()))
}

async fn body(res: Response<Incoming>) -> Result<Bytes, DockerError> {
    res.into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .map_err(|e| DockerError::Docker(e.to_string()))
}

/// Docker's error, `{"message": "..."}`.
async fn refusal(res: Response<Incoming>, id: &str) -> DockerError {
    if res.status() == StatusCode::NOT_FOUND {
        return DockerError::NoContainer(id.into());
    }
    #[derive(Deserialize)]
    struct Message {
        message: String,
    }
    let status = res.status();
    match body(res).await {
        Ok(b) => serde_json::from_slice::<Message>(&b)
            .map(|m| DockerError::Docker(m.message))
            .unwrap_or_else(|_| DockerError::Docker(status.to_string())),
        Err(e) => e,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Listed {
    id: String,
    names: Vec<String>,
    image: String,
    state: String,
    status: String,
    created: i64,
}

pub fn parse_list(json: &[u8]) -> Result<Vec<Container>, DockerError> {
    let listed: Vec<Listed> =
        serde_json::from_slice(json).map_err(|e| DockerError::Docker(e.to_string()))?;
    let mut out: Vec<Container> = listed
        .into_iter()
        .map(|c| Container {
            id: c.id.chars().take(12).collect(),
            name: c
                .names
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_default(),
            image: c.image,
            state: c.state,
            status: c.status,
            created_ms: c.created.max(0) as u64 * 1000,
        })
        .collect();
    out.sort_by(|a, b| {
        (a.state != "running")
            .cmp(&(b.state != "running"))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

pub async fn list() -> Result<Vec<Container>, DockerError> {
    let res = call(Method::GET, "/containers/json?all=true").await?;
    if !res.status().is_success() {
        return Err(refusal(res, "").await);
    }
    parse_list(&body(res).await?)
}

/// Start, stop or restart `id` (an ID or a name, checked by the caller).
pub async fn act(id: &str, action: Action) -> Result<(), DockerError> {
    let res = call(
        Method::POST,
        &format!("/containers/{id}/{}", action.as_str()),
    )
    .await?;
    // 304: it was already started or stopped.
    if res.status().is_success() || res.status() == StatusCode::NOT_MODIFIED {
        return Ok(());
    }
    Err(refusal(res, id).await)
}

/// Whether the container has a terminal: then its log is plain text, not framed.
pub async fn has_tty(id: &str) -> Result<bool, DockerError> {
    let res = call(Method::GET, &format!("/containers/{id}/json")).await?;
    if !res.status().is_success() {
        return Err(refusal(res, id).await);
    }
    let v: serde_json::Value = serde_json::from_slice(&body(res).await?)
        .map_err(|e| DockerError::Docker(e.to_string()))?;
    Ok(v["Config"]["Tty"].as_bool().unwrap_or(false))
}

/// The log stream: the last `tail` lines, then new output until the container stops.
pub async fn logs(id: &str, tail: u32) -> Result<Incoming, DockerError> {
    let path = format!("/containers/{id}/logs?follow=true&stdout=true&stderr=true&tail={tail}");
    let res = call(Method::GET, &path).await?;
    if !res.status().is_success() {
        return Err(refusal(res, id).await);
    }
    Ok(res.into_body())
}
