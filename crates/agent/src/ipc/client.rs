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

/// The first of [`Paths::sockets`] an agent answers on: one started from another kind of
/// shell may listen elsewhere than this shell's own socket.
#[cfg(unix)]
pub async fn connect(paths: &Paths) -> Result<Stream> {
    let mut first = None;
    for path in paths.sockets() {
        match tokio::net::UnixStream::connect(&path).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    Err(first.unwrap_or_else(|| std::io::ErrorKind::NotFound.into())).with_context(not_running)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// An agent started by hand in a shell with no `/run/user/<uid>` listens in its data
    /// folder; the command line in another shell must still find it.
    #[tokio::test]
    async fn finds_an_agent_listening_in_the_data_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::under(dir.path().join("data"));
        paths.runtime_dir = dir.path().join("run");
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let _agent = tokio::net::UnixListener::bind(paths.data_dir.join("pocketgpu.sock")).unwrap();
        assert!(connect(&paths).await.is_ok());
        paths.data_dir = dir.path().join("elsewhere");
        let err = connect(&paths).await.unwrap_err();
        assert!(err.to_string().contains("not running"), "{err:#}");
    }
}
