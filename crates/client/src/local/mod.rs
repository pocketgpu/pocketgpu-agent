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
        let stream = connect_any(socket_paths()).await?;
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

/// The first of `paths` an agent answers on.
#[cfg(unix)]
async fn connect_any(paths: Vec<PathBuf>) -> Result<Stream, LocalError> {
    for path in paths {
        if let Ok(stream) = tokio::net::UnixStream::connect(&path).await {
            return Ok(stream);
        }
    }
    Err(LocalError::NotRunning)
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
    socket_paths().into_iter().next()
}

/// Where an agent may listen, in the order to try them: [`socket_path`] first, then where an
/// agent started from another kind of shell would (an older agent, or `pgpu agent` run by hand
/// before there was a `/run/user/<uid>`).
#[cfg(unix)]
pub fn socket_paths() -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("PGPU_DATA_DIR") {
        return vec![PathBuf::from(dir).join("pocketgpu.sock")];
    }
    let Some(dirs) = project_dirs() else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    let folders = [
        Some(runtime_dir_of(&dirs)),
        dirs.runtime_dir().map(std::path::Path::to_path_buf),
        Some(dirs.data_local_dir().to_path_buf()),
    ];
    for path in folders
        .into_iter()
        .flatten()
        .map(|d| d.join("pocketgpu.sock"))
    {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// The folder for the agent's local socket, for the agent and the command line alike.
pub fn runtime_dir() -> Option<PathBuf> {
    project_dirs().map(|d| runtime_dir_of(&d))
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("app", "pocketgpu", "pocketgpu")
}

/// On Linux, `/run/user/<uid>/pocketgpu` whenever that user folder is there: the systemd user
/// service always runs with it, while a shell from `su - user` or `sudo -u user` has no
/// `XDG_RUNTIME_DIR` and WSL sets another one, so the command line would miss the service.
/// Else `$XDG_RUNTIME_DIR/pocketgpu`, else the data folder (macOS).
fn runtime_dir_of(dirs: &directories::ProjectDirs) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        if let Some(run) = own_dir(PathBuf::from(format!("/run/user/{uid}")), uid) {
            return run.join("pocketgpu");
        }
    }
    dirs.runtime_dir()
        .unwrap_or_else(|| dirs.data_local_dir())
        .to_path_buf()
}

/// `dir` when it is a folder that belongs to `uid`.
#[cfg(any(target_os = "linux", all(target_os = "macos", test)))]
fn own_dir(dir: PathBuf, uid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(&dir).ok()?;
    (meta.is_dir() && meta.uid() == uid).then_some(dir)
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

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn uses_the_users_run_folder_only_when_it_is_theirs() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let run = dir.path().to_path_buf();
        assert_eq!(own_dir(run.clone(), uid), Some(run.clone()));
        assert_eq!(own_dir(run.clone(), uid.wrapping_add(1)), None);
        assert_eq!(own_dir(run.join("missing"), uid), None);
        std::fs::write(run.join("file"), b"").unwrap();
        assert_eq!(own_dir(run.join("file"), uid), None);
    }

    #[tokio::test]
    async fn finds_an_agent_on_a_later_socket() {
        let dir = tempfile::tempdir().unwrap();
        let there = dir.path().join("b.sock");
        let _agent = tokio::net::UnixListener::bind(&there).unwrap();
        let paths = vec![dir.path().join("a.sock"), there];
        assert!(connect_any(paths).await.is_ok());
        assert!(matches!(
            connect_any(vec![dir.path().join("c.sock")]).await,
            Err(LocalError::NotRunning)
        ));
    }
}
