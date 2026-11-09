//! The desktop's own clipboard (F41), through arboard. One long-lived thread owns the
//! clipboard and serves requests over a channel: on X11 text stays pasteable only while its
//! owner is alive, and Windows lets only one thread open the clipboard at a time.

use std::panic::AssertUnwindSafe;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

/// Longest wait for an answer; a stuck X11 owner or a locked clipboard counts as failed.
const WAIT: Duration = Duration::from_secs(5);
/// After the clipboard couldn't be opened, the next try waits this long.
const RETRY: Duration = Duration::from_secs(30);

/// What the desktop's clipboard holds.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    Text(String),
    /// Empty, or not text.
    Empty,
    /// A password manager marked it secret (Windows), so it wasn't read.
    Secret,
}

#[derive(Debug, thiserror::Error)]
pub enum Fail {
    /// No clipboard to open: no display to connect to, or none on this system.
    #[error("no desktop clipboard here: {0}")]
    Unavailable(String),
    /// It is there but didn't work: another app holds it, or it took too long.
    #[error("the desktop clipboard failed: {0}")]
    Failed(String),
}

type Reply<T> = oneshot::Sender<Result<T, Fail>>;

enum Job {
    Check(Reply<()>),
    Put {
        text: String,
        secret: bool,
        reply: Reply<()>,
    },
    Get(Reply<Read>),
    Clear(Reply<()>),
}

/// Whether the clipboard can be opened.
pub async fn check() -> Result<(), Fail> {
    ask(Job::Check).await
}

/// `secret` asks clipboard history and cloud sync to skip the text (Windows, macOS, and
/// KDE-style clipboard managers on Linux).
pub async fn put(text: String, secret: bool) -> Result<(), Fail> {
    ask(|reply| Job::Put {
        text,
        secret,
        reply,
    })
    .await
}

pub async fn get() -> Result<Read, Fail> {
    ask(Job::Get).await
}

pub async fn clear() -> Result<(), Fail> {
    ask(Job::Clear).await
}

async fn ask<T>(job: impl FnOnce(Reply<T>) -> Job) -> Result<T, Fail> {
    let stopped = || Fail::Unavailable("the clipboard thread stopped".into());
    let (reply, answer) = oneshot::channel();
    worker()
        .ok_or_else(stopped)?
        .send(job(reply))
        .map_err(|_| stopped())?;
    match tokio::time::timeout(WAIT, answer).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(stopped()),
        Err(_) => Err(Fail::Failed("it didn't answer in time".into())),
    }
}

/// The thread that owns the clipboard, started on first use.
fn worker() -> Option<&'static mpsc::Sender<Job>> {
    static WORKER: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (jobs, rx) = mpsc::channel();
            let started = std::thread::Builder::new()
                .name("clipboard".into())
                .spawn(move || serve(rx));
            match started {
                Ok(_) => Some(jobs),
                Err(e) => {
                    tracing::warn!("could not start the clipboard thread: {e}");
                    None
                }
            }
        })
        .as_ref()
}

fn serve(jobs: mpsc::Receiver<Job>) {
    let mut board = Board::default();
    for job in jobs {
        // A panic in a platform library fails that one request (its reply is dropped), and
        // the clipboard is opened afresh for the next.
        if std::panic::catch_unwind(AssertUnwindSafe(|| board.run(job))).is_err() {
            tracing::warn!("the desktop clipboard failed; opening it again");
            board = Board::default();
        }
    }
}

#[derive(Default)]
struct Board {
    clipboard: Option<Clipboard>,
    /// When opening it last failed, and why.
    failed: Option<(Instant, String)>,
}

impl Board {
    fn run(&mut self, job: Job) {
        match job {
            Job::Check(reply) => {
                let _ = reply.send(self.open().map(|_| ()));
            }
            Job::Put {
                text,
                secret,
                reply,
            } => {
                let _ = reply.send(self.open().and_then(|c| set(c, text, secret)));
            }
            Job::Get(reply) => {
                let _ = reply.send(self.open().and_then(read));
            }
            Job::Clear(reply) => {
                let _ = reply.send(self.open().and_then(empty));
            }
        }
    }

    fn open(&mut self) -> Result<&mut Clipboard, Fail> {
        if self.clipboard.is_none() {
            if let Some((at, why)) = &self.failed
                && at.elapsed() < RETRY
            {
                return Err(Fail::Unavailable(why.clone()));
            }
            match new() {
                Ok(c) => {
                    self.clipboard = Some(c);
                    self.failed = None;
                }
                Err(why) => {
                    tracing::info!("no desktop clipboard, using PocketGPU's own: {why}");
                    self.failed = Some((Instant::now(), why.clone()));
                    return Err(Fail::Unavailable(why));
                }
            }
        }
        Ok(self.clipboard.as_mut().expect("opened above"))
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
use arboard::Clipboard;

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn new() -> Result<Clipboard, String> {
    Clipboard::new().map_err(|e| e.to_string())
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn set(c: &mut Clipboard, text: String, secret: bool) -> Result<(), Fail> {
    let set = c.set();
    // Monitoring covers both clipboard history and cloud sync (arboard advises it alone).
    #[cfg(windows)]
    let set = if secret {
        arboard::SetExtWindows::exclude_from_monitoring(set)
    } else {
        set
    };
    #[cfg(target_os = "macos")]
    let set = if secret {
        arboard::SetExtApple::exclude_from_history(set)
    } else {
        set
    };
    #[cfg(target_os = "linux")]
    let set = if secret {
        arboard::SetExtLinux::exclude_from_history(set)
    } else {
        set
    };
    set.text(text).map_err(fail)
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn read(c: &mut Clipboard) -> Result<Read, Fail> {
    if marked_secret() {
        return Ok(Read::Secret);
    }
    match c.get_text() {
        Ok(text) if text.is_empty() => Ok(Read::Empty),
        Ok(text) => Ok(Read::Text(text)),
        Err(arboard::Error::ContentNotAvailable) => Ok(Read::Empty),
        Err(e) => Err(fail(e)),
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn empty(c: &mut Clipboard) -> Result<(), Fail> {
    c.clear().map_err(fail)
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn fail(e: arboard::Error) -> Fail {
    match e {
        arboard::Error::ClipboardNotSupported => Fail::Unavailable(e.to_string()),
        e => Fail::Failed(e.to_string()),
    }
}

/// Whether a password manager marked what is on the clipboard as secret, the way Windows
/// documents it. Checked without opening the clipboard, so the text is never read.
#[cfg(windows)]
fn marked_secret() -> bool {
    use windows_sys::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    let name: Vec<u16> = "ExcludeClipboardContentFromMonitorProcessing"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    // SAFETY: takes a format number only.
    format != 0 && unsafe { IsClipboardFormatAvailable(format) } != 0
}

/// macOS and Linux have no mark this can see through arboard.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn marked_secret() -> bool {
    false
}

/// No desktop clipboard on other systems: everything stays in memory.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
enum Clipboard {}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn new() -> Result<Clipboard, String> {
    Err("not supported on this system".into())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn set(c: &mut Clipboard, _: String, _: bool) -> Result<(), Fail> {
    match *c {}
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn read(c: &mut Clipboard) -> Result<Read, Fail> {
    match *c {}
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn empty(c: &mut Clipboard) -> Result<(), Fail> {
    match *c {}
}
