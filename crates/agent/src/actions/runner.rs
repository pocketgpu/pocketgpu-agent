//! Runs an action through the user's shell, as the agent user, with a timeout and capped
//! output. The command gets its own process group so a timeout stops everything it started.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pocketgpu_proto::msg::control::Action;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Output kept per run (data-model: 64 KB); the end is kept, where errors usually are.
pub const MAX_OUTPUT: usize = 64 * 1024;
/// Time between SIGTERM and SIGKILL when the timeout hits.
const KILL_GRACE: Duration = Duration::from_secs(10);
const CUT_NOTE: &str = "… (earlier output cut)\n";

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// `None` when a signal or the timeout ended it.
    pub exit_code: Option<i32>,
    pub output: String,
    pub duration: Duration,
    pub timed_out: bool,
}

/// The shell actions run in: the user's login shell, or `/bin/sh`; PowerShell on Windows.
#[cfg(windows)]
fn shell() -> String {
    crate::winsys::default_shell()
}

#[cfg(not(windows))]
fn shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

pub async fn run(action: &Action) -> Result<Outcome> {
    run_with(action, Duration::from_secs(u64::from(action.timeout_s))).await
}

async fn run_with(action: &Action, timeout: Duration) -> Result<Outcome> {
    let started = Instant::now();
    let mut cmd = Command::new(shell());
    #[cfg(windows)]
    {
        // No console window for the action; PowerShell runs the command as typed.
        cmd.creation_flags(0x0800_0000)
            .args(["-NoProfile", "-NonInteractive", "-Command"]);
    }
    #[cfg(not(windows))]
    cmd.arg("-c");
    cmd.arg(&action.command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PGPU_ACTION", &action.name);
    for key in crate::AGENT_ONLY_ENV {
        cmd.env_remove(key);
    }
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
        cmd.current_dir(home);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("could not start {}", shell()))?;

    let buf = Arc::new(Mutex::new(Tail::default()));
    let readers = [
        child
            .stdout
            .take()
            .map(|o| tokio::spawn(collect(o, buf.clone()))),
        child
            .stderr
            .take()
            .map(|e| tokio::spawn(collect(e, buf.clone()))),
    ];

    let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => (status?, false),
        Err(_) => {
            let pgid = child.id();
            signal_group(pgid, Signal::Term);
            let status = match tokio::time::timeout(KILL_GRACE, child.wait()).await {
                Ok(s) => s?,
                Err(_) => {
                    signal_group(pgid, Signal::Kill);
                    child.wait().await?
                }
            };
            (status, true)
        }
    };
    // Background processes may keep the pipes open; don't wait for them forever.
    for r in readers.into_iter().flatten() {
        let _ = tokio::time::timeout(Duration::from_secs(1), r).await;
    }

    let mut output = buf.lock().expect("output lock poisoned").text();
    if timed_out {
        output.push_str(&format!(
            "\n(stopped after the {} s timeout)\n",
            timeout.as_secs()
        ));
    }
    Ok(Outcome {
        exit_code: if timed_out { None } else { status.code() },
        output,
        duration: started.elapsed(),
        timed_out,
    })
}

async fn collect(mut r: impl AsyncRead + Unpin, buf: Arc<Mutex<Tail>>) {
    let mut chunk = [0u8; 8192];
    while let Ok(n) = r.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        buf.lock().expect("output lock poisoned").push(&chunk[..n]);
    }
}

/// The last [`MAX_OUTPUT`] bytes of output.
#[derive(Debug, Default)]
struct Tail {
    bytes: Vec<u8>,
    cut: bool,
}

impl Tail {
    fn push(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
        if self.bytes.len() > MAX_OUTPUT {
            let extra = self.bytes.len() - MAX_OUTPUT;
            self.bytes.drain(..extra);
            self.cut = true;
        }
    }

    fn text(&self) -> String {
        let text = String::from_utf8_lossy(&self.bytes);
        if !self.cut {
            return text.into_owned();
        }
        // Start at a line break so the first line isn't half a line.
        let from = text.find('\n').map(|i| i + 1).unwrap_or(0);
        format!("{CUT_NOTE}{}", &text[from..])
    }
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: Option<u32>, sig: Signal) {
    let Some(pgid) = pgid else { return };
    let sig = match sig {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: killpg has no memory-safety preconditions.
    unsafe {
        libc::killpg(pgid as libc::pid_t, sig);
    }
}

/// Windows: the group id is the shell's PID; ending its tree ends what it started.
#[cfg(windows)]
fn signal_group(pgid: Option<u32>, sig: Signal) {
    if let Some(pid) = pgid {
        let _ = crate::winsys::kill_tree(pid, matches!(sig, Signal::Kill));
    }
}

#[cfg(not(any(unix, windows)))]
fn signal_group(_pgid: Option<u32>, _sig: Signal) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn action(command: &str) -> Action {
        Action {
            id: 1,
            name: "Test".into(),
            command: command.into(),
            icon: "zap".into(),
            risky: false,
            timeout_s: 5,
        }
    }

    /// The same command for sh (Linux) and PowerShell (Windows).
    fn shell(unix: &str, windows: &str) -> String {
        (if cfg!(windows) { windows } else { unix }).to_string()
    }

    #[tokio::test]
    async fn captures_output_and_exit_code() {
        let cmd = shell(
            "echo out; echo err >&2; exit 3",
            "echo out; [Console]::Error.WriteLine('err'); exit 3",
        );
        let out = run(&action(&cmd)).await.unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert!(out.output.contains("out"));
        assert!(out.output.contains("err"));
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn stops_at_the_timeout() {
        let out = run_with(
            &action(&shell(
                "echo started; sleep 30",
                "echo started; Start-Sleep 30",
            )),
            Duration::from_millis(300),
        )
        .await
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
        assert!(out.output.contains("started"));
        assert!(out.duration < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn keeps_only_the_end_of_long_output() {
        let out = run(&action(&shell("seq 1 50000", "1..50000")))
            .await
            .unwrap();
        assert!(out.output.len() <= MAX_OUTPUT + CUT_NOTE.len());
        assert!(out.output.starts_with(CUT_NOTE));
        assert!(out.output.trim_end().ends_with("50000"));
    }
}
