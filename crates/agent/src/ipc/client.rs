//! The command line's side of the local socket.

use anyhow::{Context, Result, bail};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::{read_frame, write_frame};

use crate::store::paths::Paths;

pub async fn call(paths: &Paths, req: Request) -> Result<Response> {
    let mut stream = connect(paths).await?;
    write_frame(&mut stream, &req).await?;
    match read_frame(&mut stream).await? {
        Some(resp) => Ok(resp),
        None => bail!("the agent closed the connection"),
    }
}

/// The local socket stream.
#[cfg(unix)]
pub type Stream = tokio::net::UnixStream;
#[cfg(windows)]
pub type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
pub async fn connect(paths: &Paths) -> Result<Stream> {
    let path = paths.socket();
    tokio::net::UnixStream::connect(&path)
        .await
        .with_context(not_running)
}

/// A busy pipe means another client is connecting right now, not that the agent is gone.
/// A pipe served by another user's program is never talked to (it could be squatting the name).
#[cfg(windows)]
pub async fn connect(_paths: &Paths) -> Result<Stream> {
    use pocketgpu_client::local::winpipe;
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut tries = 0;
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name()) {
            Ok(s) if !winpipe::server_is_ours(&s) => {
                bail!("another user's program holds the PocketGPU pipe; not talking to it")
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 40 => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            other => return other.with_context(not_running),
        }
    }
}

#[cfg(windows)]
pub fn pipe_name() -> String {
    pocketgpu_client::local::pipe_name()
}

fn not_running() -> String {
    "the PocketGPU agent is not running. Start it with `pgpu service install` or `pgpu agent`"
        .into()
}
