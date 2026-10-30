//! The agent on this machine, over its local socket (a named pipe on Windows), as the
//! command line reaches it. For the desktop tray app (F22); never over the network.

use std::path::PathBuf;

#[cfg(windows)]
pub mod winpipe;

use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::{FrameError, read_frame, write_frame};

#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("the PocketGPU agent is not running on this machine")]
    NotRunning,
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("the agent closed the connection")]
    Closed,
    #[error("another user's program holds the PocketGPU pipe; not talking to it")]
    NotOurs,
}

#[cfg(unix)]
type Stream = tokio::net::UnixStream;
#[cfg(windows)]
type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

/// One open connection; pairing keeps it for the whole exchange.
pub struct LocalStream(Stream);

impl LocalStream {
    pub async fn connect() -> Result<Self, LocalError> {
        #[cfg(unix)]
        let stream = tokio::net::UnixStream::connect(socket_path().ok_or(LocalError::NotRunning)?)
            .await
            .map_err(|_| LocalError::NotRunning)?;
        #[cfg(windows)]
        let stream = open_pipe().await?;
        Ok(Self(stream))
    }

    pub async fn send(&mut self, req: &Request) -> Result<(), LocalError> {
        Ok(write_frame(&mut self.0, req).await?)
    }

    pub async fn recv(&mut self) -> Result<Response, LocalError> {
        read_frame(&mut self.0).await?.ok_or(LocalError::Closed)
    }
}

/// One request and its reply.
pub async fn call(req: &Request) -> Result<Response, LocalError> {
    let mut s = LocalStream::connect().await?;
    s.send(req).await?;
    s.recv().await
}

/// Where the agent listens: the same rules as the agent's own `Paths`.
#[cfg(unix)]
pub fn socket_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PGPU_DATA_DIR") {
        return Some(PathBuf::from(dir).join("pocketgpu.sock"));
    }
    let dirs = directories::ProjectDirs::from("app", "pocketgpu", "pocketgpu")?;
    let dir = dirs
        .runtime_dir()
        .unwrap_or_else(|| dirs.data_local_dir())
        .to_path_buf();
    Some(dir.join("pocketgpu.sock"))
}

/// The agent serves one new connection at a time: while another client is connecting the pipe
/// is busy, which doesn't mean the agent is gone. Wait for it a little. The server must run as
/// this user ([`winpipe::server_is_ours`]).
#[cfg(windows)]
async fn open_pipe() -> Result<Stream, LocalError> {
    const ERROR_PIPE_BUSY: i32 = 231;
    for _ in 0..40 {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name()) {
            Ok(s) if winpipe::server_is_ours(&s) => return Ok(s),
            Ok(_) => return Err(LocalError::NotOurs),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await
            }
            Err(_) => return Err(LocalError::NotRunning),
        }
    }
    Err(LocalError::NotRunning)
}

/// The file in the agent's data folder (private to the user) with the random part of the
/// pipe name. A name others can predict could be taken first by another user on the PC,
/// which would keep the agent from starting.
#[cfg(windows)]
pub const PIPE_TAG_FILE: &str = "pipe-name";

/// `\\.\pipe\pocketgpu-<user>-<random>`; without the agent's tag file (an agent before
/// 0.1.0-rc.4), the old `\\.\pipe\pocketgpu-<user>`.
#[cfg(windows)]
pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let tag = data_dir()
        .and_then(|d| std::fs::read_to_string(d.join(PIPE_TAG_FILE)).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()));
    match tag {
        Some(tag) => format!(r"\\.\pipe\pocketgpu-{user}-{tag}"),
        None => format!(r"\\.\pipe\pocketgpu-{user}"),
    }
}

/// Where the agent keeps job logs and its own log, for "Open logs folder".
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PGPU_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("app", "pocketgpu", "pocketgpu")
        .map(|d| d.data_local_dir().to_path_buf())
}
