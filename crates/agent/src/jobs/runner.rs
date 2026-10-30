//! `pgpu run [--name N] [--no-alert] -- <command>`: runs the command as a watched job.
//!
//! Output and the exit code pass through unchanged. On a terminal the command gets a
//! pseudo-terminal (progress bars keep working) and keys go to it as typed; otherwise its
//! output is piped. Either way it is logged for the phone. Without a running agent the
//! command still runs, with one warning that nothing is watched. Windows always pipes: Ctrl+C
//! reaches the command through the shared console, and a stop ends its process tree.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use pocketgpu_proto::local::{JobStart, Request, Response};
use pocketgpu_proto::write_frame;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::STOP_GRACE;
use super::store::LOG_CAP_BYTES;
use crate::cli::Silent;
use crate::ipc::client;
use crate::store::paths::Paths;

pub struct RunOptions {
    pub name: Option<String>,
    pub no_alert: bool,
    pub command: Vec<String>,
}

pub async fn run(paths: &Paths, json: bool, opts: RunOptions) -> Result<()> {
    let Some(program) = opts.command.first() else {
        bail!("nothing to run. Usage: pgpu run -- <command>");
    };
    let cwd = std::env::current_dir().context("could not read the current folder")?;
    let name = opts.name.clone().unwrap_or_else(|| job_name(&opts.command));
    let start = JobStart {
        name: name.clone(),
        command: command_text(&opts.command),
        cwd: cwd.to_string_lossy().into_owned(),
        alerts: !opts.no_alert,
    };
    let mut session = Session::open(paths, start).await;
    let log = match &session {
        Some(s) => Some(Arc::new(Mutex::new(LogFile::create(&s.log_path)?))),
        None => {
            eprintln!(
                "pgpu: the agent isn't running, so this job isn't watched and no alerts will be sent"
            );
            None
        }
    };
    if let Some(s) = &session {
        if json {
            println!("{}", serde_json::json!({ "job_id": s.id }));
        } else {
            eprintln!("▶ Job {} \"{name}\" started", s.id);
        }
    }

    let started = Instant::now();
    #[cfg(unix)]
    let (status, stopped) = if is_terminal(0) && is_terminal(1) {
        run_in_pty(program, &opts.command[1..], log, session.as_mut()).await?
    } else {
        run_piped(program, &opts.command[1..], log, session.as_mut()).await?
    };
    #[cfg(not(unix))]
    let (status, stopped) = run_piped(program, &opts.command[1..], log, session.as_mut()).await?;
    let code = exit_code(status);

    if let Some(s) = session {
        let id = s.id;
        s.end(Some(code), stopped).await;
        if !json {
            let took = elapsed(started.elapsed());
            match (stopped, code) {
                (true, _) => eprintln!("■ Job {id} \"{name}\" stopped after {took}"),
                (false, 0) => eprintln!("✓ Job {id} \"{name}\" finished in {took}"),
                (false, c) => eprintln!("✗ Job {id} \"{name}\" crashed after {took} (exit {c})"),
            }
        }
    }
    if code == 0 {
        Ok(())
    } else {
        Err(Silent(code.clamp(1, 255) as u8).into())
    }
}

/// The local-socket session with the agent for one job.
struct Session {
    id: u64,
    log_path: PathBuf,
    write: tokio::io::WriteHalf<client::Stream>,
    replies: mpsc::Receiver<Response>,
}

impl Session {
    async fn open(paths: &Paths, start: JobStart) -> Option<Self> {
        let stream = client::connect(paths).await.ok()?;
        let (reader, mut write) = tokio::io::split(stream);
        write_frame(&mut write, &Request::JobStart(start))
            .await
            .ok()?;
        let mut replies = crate::ipc::forward_frames::<_, Response>(reader);
        match tokio::time::timeout(Duration::from_secs(5), replies.recv()).await {
            Ok(Some(Response::JobStarted { id, log_path })) => Some(Self {
                id,
                log_path: PathBuf::from(log_path),
                write,
                replies,
            }),
            Ok(Some(Response::Error(e))) => {
                eprintln!("pgpu: {e}");
                None
            }
            _ => None,
        }
    }

    async fn report_group(&mut self, pgid: u32) {
        let _ = write_frame(&mut self.write, &Request::JobProcess { pgid }).await;
    }

    async fn end(mut self, exit_code: Option<i32>, stopped: bool) {
        let end = Request::JobEnd { exit_code, stopped };
        if write_frame(&mut self.write, &end).await.is_ok() {
            // Wait briefly for the agent to record it, so `pgpu jobs` right after is current.
            let _ = tokio::time::timeout(Duration::from_secs(2), self.replies.recv()).await;
        }
    }
}

#[cfg(unix)]
/// Waits for the job while handling stop requests from the agent and signals sent to us.
async fn supervise(
    pgid: u32,
    wait: impl Future<Output = std::io::Result<ExitStatus>>,
    mut session: Option<&mut Session>,
) -> Result<(ExitStatus, bool)> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut int = signal(SignalKind::interrupt())?;
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut stopped = false;
    let mut kill_at: Option<tokio::time::Instant> = None;
    tokio::pin!(wait);
    loop {
        let stop_request = async {
            match session.as_mut() {
                Some(s) => s.replies.recv().await,
                None => std::future::pending().await,
            }
        };
        let kill_timer = async {
            match kill_at {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            status = &mut wait => return Ok((status?, stopped)),
            msg = stop_request => match msg {
                Some(Response::StopJob) if !stopped => {
                    stopped = true;
                    super::pty::signal_group(pgid, libc::SIGTERM);
                    kill_at = Some(tokio::time::Instant::now() + STOP_GRACE);
                }
                Some(_) => {}
                // The agent went away: the job keeps running, just unwatched.
                None => session = None,
            },
            _ = kill_timer => {
                super::pty::signal_group(pgid, libc::SIGKILL);
                kill_at = None;
            }
            _ = int.recv() => super::pty::signal_group(pgid, libc::SIGINT),
            _ = term.recv() => super::pty::signal_group(pgid, libc::SIGTERM),
            _ = hup.recv() => super::pty::signal_group(pgid, libc::SIGHUP),
        }
    }
}

/// Windows: Ctrl+C reaches the command through the console we share, so here it only must
/// not end us first. A stop ends the process tree, forcefully after the grace time.
#[cfg(windows)]
async fn supervise(
    pid: u32,
    wait: impl Future<Output = std::io::Result<ExitStatus>>,
    mut session: Option<&mut Session>,
) -> Result<(ExitStatus, bool)> {
    let mut stopped = false;
    let mut kill_at: Option<tokio::time::Instant> = None;
    tokio::pin!(wait);
    loop {
        let stop_request = async {
            match session.as_mut() {
                Some(s) => s.replies.recv().await,
                None => std::future::pending().await,
            }
        };
        let kill_timer = async {
            match kill_at {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            status = &mut wait => return Ok((status?, stopped)),
            msg = stop_request => match msg {
                Some(Response::StopJob) if !stopped => {
                    stopped = true;
                    if crate::winsys::kill_tree(pid, false).is_err() {
                        let _ = crate::winsys::kill_tree(pid, true);
                    }
                    kill_at = Some(tokio::time::Instant::now() + STOP_GRACE);
                }
                Some(_) => {}
                None => session = None,
            },
            _ = kill_timer => {
                let _ = crate::winsys::kill_tree(pid, true);
                kill_at = None;
            }
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}

type SharedLog = Option<Arc<Mutex<LogFile>>>;

/// Output piped: stdout and stderr each go to ours and to the log.
async fn run_piped(
    program: &str,
    args: &[String],
    log: SharedLog,
    mut session: Option<&mut Session>,
) -> Result<(ExitStatus, bool)> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {program}"))?;
    let pgid = child.id().context("the command exited at once")?;
    if let Some(s) = session.as_mut() {
        s.report_group(pgid).await;
    }
    let out = tokio::spawn(tee(child.stdout.take(), tokio::io::stdout(), log.clone()));
    let err = tokio::spawn(tee(child.stderr.take(), tokio::io::stderr(), log));
    let result = supervise(pgid, child.wait(), session).await;
    // Output still in the pipes; don't wait forever on processes the job left behind.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = out.await;
        let _ = err.await;
    })
    .await;
    result
}

async fn tee<R, W>(from: Option<R>, mut to: W, log: SharedLog)
where
    R: AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let Some(mut from) = from else { return };
    let mut buf = vec![0u8; 16 * 1024];
    while let Ok(n) = from.read(&mut buf).await {
        if n == 0 {
            break;
        }
        let _ = to.write_all(&buf[..n]).await;
        let _ = to.flush().await;
        if let Some(l) = &log {
            l.lock().expect("log lock poisoned").write(&buf[..n]);
        }
    }
}

/// Output through a pseudo-terminal, with our terminal in raw mode meanwhile.
#[cfg(unix)]
async fn run_in_pty(
    program: &str,
    args: &[String],
    log: SharedLog,
    mut session: Option<&mut Session>,
) -> Result<(ExitStatus, bool)> {
    use super::pty::{Pty, RawMode, copy_size};
    use std::os::fd::AsRawFd;

    let pty = Pty::open(1).context("could not open a pseudo-terminal")?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    let (mut child, master) = pty
        .spawn(cmd)
        .with_context(|| format!("could not run {program}"))?;
    let pgid = child.id();
    if let Some(s) = session.as_mut() {
        s.report_group(pgid).await;
    }
    let raw = RawMode::enable(0).ok();

    // Job output → our terminal and the log, until the pseudo-terminal closes.
    let mut from_job = master.try_clone()?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = vec![0u8; 16 * 1024];
        let mut stdout = std::io::stdout();
        while let Ok(n) = from_job.read(&mut buf) {
            if n == 0 {
                break;
            }
            let _ = stdout.write_all(&buf[..n]);
            let _ = stdout.flush();
            if let Some(l) = &log {
                l.lock().expect("log lock poisoned").write(&buf[..n]);
            }
        }
        let _ = done_tx.send(());
    });
    // Keys → the job. This thread may stay blocked on stdin; the process exits anyway.
    let mut to_job = master.try_clone()?;
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 1024];
        let mut stdin = std::io::stdin();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || to_job.write_all(&buf[..n]).is_err() {
                break;
            }
        }
    });
    // Window resizes follow ours.
    let master_fd = master.as_raw_fd();
    let resize = tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut winch) = signal(SignalKind::window_change()) {
            while winch.recv().await.is_some() {
                copy_size(1, master_fd);
            }
        }
    });

    let wait = async { tokio::task::spawn_blocking(move || child.wait()).await? };
    let result = supervise(pgid, wait, session).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), done_rx).await;
    resize.abort();
    drop(raw);
    drop(master);
    result
}

/// The job's log, capped at [`LOG_CAP_BYTES`]: past the cap, the older half is dropped so
/// the end of the output is kept.
struct LogFile {
    file: std::fs::File,
    path: PathBuf,
    len: u64,
}

impl LogFile {
    fn create(path: &Path) -> Result<Self> {
        // Not append mode: Windows can't shorten a file opened for appending only, which
        // `keep_end` needs. The single writer keeps the position at the end itself.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("could not open the job log {}", path.display()))?;
        let len = std::io::Seek::seek(&mut file, std::io::SeekFrom::End(0)).unwrap_or(0);
        Ok(Self {
            file,
            path: path.to_path_buf(),
            len,
        })
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.len + bytes.len() as u64 > LOG_CAP_BYTES {
            self.keep_end(LOG_CAP_BYTES / 2);
        }
        if self.file.write_all(bytes).is_ok() {
            self.len += bytes.len() as u64;
        }
    }

    fn keep_end(&mut self, keep: u64) {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return;
        };
        let start = self.len.saturating_sub(keep);
        let mut tail = Vec::with_capacity(keep as usize);
        if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut tail).is_err() {
            return;
        }
        let rewound = self.file.set_len(0).is_ok() && self.file.seek(SeekFrom::Start(0)).is_ok();
        if rewound && self.file.write_all(&tail).is_ok() {
            self.len = tail.len() as u64;
        }
    }
}

/// The exit code a shell would report: the code, or 128 + the signal.
#[cfg(windows)]
pub fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

#[cfg(unix)]
pub fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// `--name`, else the script for interpreters (`python train.py` → `train.py`), else the
/// program.
pub fn job_name(command: &[String]) -> String {
    const RUNNERS: [&str; 16] = [
        "python",
        "node",
        "bash",
        "sh",
        "zsh",
        "uv",
        "uvx",
        "poetry",
        "accelerate",
        "torchrun",
        "deepspeed",
        "bun",
        "deno",
        "ruby",
        "perl",
        "npx",
    ];
    const SUBCOMMANDS: [&str; 3] = ["run", "launch", "exec"];
    let base = |s: &str| {
        Path::new(s)
            .file_name()
            .map_or_else(|| s.to_string(), |n| n.to_string_lossy().into_owned())
    };
    let Some(program) = command.first() else {
        return "job".into();
    };
    let prog = base(program);
    let is_runner = RUNNERS
        .iter()
        .any(|r| prog == *r || prog.starts_with("python"));
    if is_runner
        && let Some(script) = command[1..]
            .iter()
            .find(|a| !a.starts_with('-') && !SUBCOMMANDS.contains(&a.as_str()))
        // Inline code such as `sh -c "…"` isn't a script name.
        && !script.contains(char::is_whitespace)
    {
        return base(script);
    }
    prog
}

/// The command as a person would type it, quoting arguments with spaces or quotes.
pub fn command_text(command: &[String]) -> String {
    command
        .iter()
        .map(|a| {
            if a.is_empty()
                || a.chars()
                    .any(|c| c.is_whitespace() || "'\"$`\\".contains(c))
            {
                format!("'{}'", a.replace('\'', r"'\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `2h 14m`, `3m 20s` or `12s`.
pub fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

#[cfg(unix)]
fn is_terminal(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn names_come_from_scripts() {
        assert_eq!(job_name(&cmd("python train.py --epochs 20")), "train.py");
        assert_eq!(
            job_name(&cmd("/usr/bin/python3.12 -u scripts/eval.py")),
            "eval.py"
        );
        assert_eq!(job_name(&cmd("uv run finetune.py")), "finetune.py");
        assert_eq!(job_name(&cmd("./runs/next.sh --resume")), "next.sh");
        assert_eq!(job_name(&cmd("ollama pull qwen3:14b")), "ollama");
        let inline = vec!["sh".into(), "-c".into(), "echo hi; exit 3".into()];
        assert_eq!(job_name(&inline), "sh");
    }

    #[test]
    fn commands_read_like_typed() {
        let c = vec!["sh".into(), "-c".into(), "sleep 1; exit 2".into()];
        assert_eq!(command_text(&c), "sh -c 'sleep 1; exit 2'");
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(elapsed(Duration::from_secs(12)), "12s");
        assert_eq!(elapsed(Duration::from_secs(200)), "3m 20s");
        assert_eq!(elapsed(Duration::from_secs(8040)), "2h 14m");
    }

    #[test]
    fn log_keeps_the_end_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1.log");
        let mut log = LogFile::create(&path).unwrap();
        log.write(b"0123456789");
        log.keep_end(4);
        log.write(b"ab");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "6789ab");
    }
}
