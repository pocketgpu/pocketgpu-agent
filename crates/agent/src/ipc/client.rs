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

/// How long [`call_stream`] waits for the agent's first reply: every tool call waits this
/// at most when nothing is held.
pub const FIRST_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
/// How long it then waits for a held action's answer: past the agent's 5 minutes, inside
/// the hook entry's 330 s.
pub const FINAL_WAIT: std::time::Duration = std::time::Duration::from_secs(310);

/// The two replies of an AI assistant check (F32): the first within [`FIRST_WAIT`] and, when
/// it is `AssistantHeld`, the final one within [`FINAL_WAIT`]. Any error, a closed stream or
/// a timeout is an `Err`: the hook then answers as if the agent were down. [`call`] stays
/// one reply.
pub async fn call_stream(paths: &Paths, req: Request) -> Result<(Response, Option<Response>)> {
    call_stream_within(paths, req, FIRST_WAIT, FINAL_WAIT).await
}

/// The error [`call_stream`] gives when the action was held and then the answer never came
/// (the agent stopped, the stream closed, the clock ran out): never a reason to let it go.
#[derive(Debug)]
pub struct HeldLost;

impl std::fmt::Display for HeldLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the action was held and its answer never came")
    }
}

impl std::error::Error for HeldLost {}

pub async fn call_stream_within(
    paths: &Paths,
    req: Request,
    first_wait: std::time::Duration,
    final_wait: std::time::Duration,
) -> Result<(Response, Option<Response>)> {
    use tokio::time::timeout;
    let first = timeout(first_wait, async {
        let mut stream = connect(paths).await?;
        write_frame(&mut stream, &req).await?;
        let first: Option<Response> = read_frame(&mut stream).await?;
        anyhow::Ok((stream, first))
    })
    .await
    .context("the agent didn't answer in time")??;
    let (mut stream, first) = first;
    let Some(first) = first else {
        bail!("the agent closed the connection");
    };
    if !matches!(first, Response::AssistantHeld { .. }) {
        return Ok((first, None));
    }
    match timeout(final_wait, read_frame(&mut stream)).await {
        Ok(Ok(Some(last))) => Ok((first, Some(last))),
        _ => Err(HeldLost.into()),
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

    /// A fake agent on a socket that answers each connection with these replies, then
    /// stays quiet (or closes when `close`).
    fn fake_agent(paths: &Paths, replies: Vec<Response>, close: bool) {
        std::fs::create_dir_all(&paths.runtime_dir).unwrap();
        let listener = tokio::net::UnixListener::bind(paths.socket()).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _: Option<Request> = read_frame(&mut s).await.unwrap();
            for r in &replies {
                write_frame(&mut s, r).await.unwrap();
            }
            if !close {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
    }

    fn check() -> Request {
        Request::AssistantCheck {
            assistant: "claude".into(),
            session: "s".into(),
            permission_mode: "default".into(),
            tool: "Bash".into(),
            input: "rm x".into(),
            oversize: false,
            folder: "/".into(),
        }
    }

    #[tokio::test]
    async fn call_stream_reads_one_or_two_replies() {
        use pocketgpu_proto::assistants::Answer;
        use std::time::Duration;
        let ms = Duration::from_millis;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path().join("a"));
        fake_agent(&paths, vec![Response::AssistantAnswer(Answer::Pass)], false);
        let (first, last) = call_stream_within(&paths, check(), ms(1000), ms(1000))
            .await
            .unwrap();
        assert_eq!(first, Response::AssistantAnswer(Answer::Pass));
        assert_eq!(last, None);

        let paths = Paths::under(dir.path().join("b"));
        let held = Response::AssistantHeld { id: 9 };
        let allow = Response::AssistantAnswer(Answer::Allow);
        fake_agent(&paths, vec![held.clone(), allow.clone()], false);
        let got = call_stream_within(&paths, check(), ms(1000), ms(1000))
            .await
            .unwrap();
        assert_eq!(got, (held.clone(), Some(allow)));

        // Held, then nothing: an error once the final wait is over, never an answer.
        let paths = Paths::under(dir.path().join("c"));
        fake_agent(&paths, vec![held.clone()], false);
        let lost = |r: Result<(Response, Option<Response>)>| {
            r.is_err_and(|e| e.downcast_ref::<HeldLost>().is_some())
        };
        assert!(lost(
            call_stream_within(&paths, check(), ms(1000), ms(100)).await
        ));
        // Held, then closed (an agent stopping): an error too.
        let paths = Paths::under(dir.path().join("d"));
        fake_agent(&paths, vec![held], true);
        assert!(lost(
            call_stream_within(&paths, check(), ms(1000), ms(1000)).await
        ));
        // No first reply in time (an older agent busy or stuck): an error.
        let paths = Paths::under(dir.path().join("e"));
        fake_agent(&paths, vec![], false);
        let r = call_stream_within(&paths, check(), ms(100), ms(1000)).await;
        assert!(r.is_err() && !lost(r));
        // An older agent closes the stream on the unknown tag: an error.
        let paths = Paths::under(dir.path().join("f"));
        fake_agent(&paths, vec![], true);
        assert!(
            call_stream_within(&paths, check(), ms(1000), ms(1000))
                .await
                .is_err()
        );
    }
}
