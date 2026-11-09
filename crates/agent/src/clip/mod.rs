//! The clipboard for other devices (F41): the desktop's own clipboard when the agent runs
//! in a session with a screen, else PocketGPU's own one in memory, never written to disk
//! and cleared after 10 minutes. Off until `pgpu clip on` at the machine. Phones and the
//! command line use the same place.

pub mod handler;
mod memory;
mod system;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use iroh::PublicKey;
use pocketgpu_proto::clip::{CLEARS_AFTER_S, Info, MAX_TEXT, Place};
use pocketgpu_proto::local::{ClipStatus, Request, Response};
use pocketgpu_proto::{Error, ErrorCode};

use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use memory::{CLEARS_AFTER, Memory};
use system::{Fail, Read};

/// Where text goes on this machine: the desktop's clipboard on Windows and macOS, and on
/// Linux when there is a display; else PocketGPU's own in memory. `PGPU_CLIPBOARD=memory`
/// always picks memory, so tests never touch the desktop's clipboard.
pub fn place() -> Place {
    if std::env::var("PGPU_CLIPBOARD").is_ok_and(|v| v == "memory") {
        return Place::Memory;
    }
    if cfg!(any(windows, target_os = "macos")) || cfg!(target_os = "linux") && display() {
        Place::System
    } else {
        Place::Memory
    }
}

/// Whether a display is named to connect to (Linux).
fn display() -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|d| !d.is_empty()))
}

/// Each agent's in-memory clipboard, by agent ID: tests run several agents in one process.
static MEMORY: LazyLock<Mutex<HashMap<PublicKey, Memory>>> = LazyLock::new(Default::default);

fn memory<R>(agent: &Agent, f: impl FnOnce(&mut Memory) -> R) -> R {
    memory_of(agent.identity.agent_id(), f)
}

fn memory_of<R>(agent_id: PublicKey, f: impl FnOnce(&mut Memory) -> R) -> R {
    let mut all = MEMORY.lock().expect("clipboard lock poisoned");
    f(all.entry(agent_id).or_default())
}

fn enabled(agent: &Agent) -> bool {
    agent.config.read().expect("config lock poisoned").clipboard
}

/// Where text goes right now: memory when the desktop's clipboard can't be opened, or
/// while memory holds text a failed put left there.
async fn current(agent: &Agent) -> Place {
    if place() == Place::Memory || memory(agent, |m| m.has_text(Instant::now())) {
        return Place::Memory;
    }
    match system::check().await {
        Ok(()) => Place::System,
        Err(_) => Place::Memory,
    }
}

async fn info(agent: &Agent) -> Info {
    let place = current(agent).await;
    Info {
        enabled: enabled(agent),
        place,
        clears_after_s: (place == Place::Memory).then_some(CLEARS_AFTER_S),
    }
}

/// Text that may be put: not empty, at most [`MAX_TEXT`] bytes.
fn check(text: &str) -> Result<(), Error> {
    if text.is_empty() {
        return Err(Error::new(ErrorCode::Invalid, "There is no text to put."));
    }
    if text.len() > MAX_TEXT {
        return Err(too_long());
    }
    Ok(())
}

fn too_long() -> Error {
    Error::new(
        ErrorCode::Invalid,
        format!(
            "The clipboard takes at most {} KB of text.",
            MAX_TEXT / 1024
        ),
    )
}

/// Puts checked text where it goes now and says where. When the desktop's clipboard
/// fails, the text goes to memory instead, and [`current`] says so until it is gone. `once`
/// only applies in memory.
async fn put(agent: &Agent, text: String, secret: bool, once: bool) -> Place {
    if place() == Place::System {
        match system::put(text.clone(), secret).await {
            Ok(()) => {
                memory(agent, Memory::clear);
                return Place::System;
            }
            Err(e @ Fail::Unavailable(_)) => tracing::debug!("keeping the text in memory: {e}"),
            Err(e) => tracing::warn!("keeping the text in memory: {e}"),
        }
    }
    memory(agent, |m| m.put(text, once, Instant::now()));
    forget_later(agent.identity.agent_id());
    Place::Memory
}

/// Drops the text when it is due, even if nobody asks for it again.
fn forget_later(agent_id: PublicKey) {
    tokio::spawn(async move {
        tokio::time::sleep(CLEARS_AFTER).await;
        memory_of(agent_id, |m| m.expire(Instant::now()));
    });
}

/// What a read found, and where; `age` only for memory.
struct Found {
    place: Place,
    read: Read,
    age: Option<Duration>,
}

/// Reads where text goes now; a `once` text is gone after this.
async fn get(agent: &Agent) -> Result<Found, Error> {
    if place() == Place::System && !memory(agent, |m| m.has_text(Instant::now())) {
        match system::get().await {
            Ok(Read::Text(text)) if text.len() > MAX_TEXT => return Err(too_long()),
            Ok(read) => {
                return Ok(Found {
                    place: Place::System,
                    read,
                    age: None,
                });
            }
            // Nothing could have been put there, and memory is empty.
            Err(Fail::Unavailable(_)) => {}
            Err(e) => {
                tracing::warn!("could not read the clipboard: {e}");
                return Err(Error::new(
                    ErrorCode::Busy,
                    "Could not read the clipboard on this machine. Try again.",
                ));
            }
        }
    }
    let held = memory(agent, |m| m.get(Instant::now()));
    Ok(Found {
        place: Place::Memory,
        age: held.as_ref().map(|(_, age)| *age),
        read: held.map_or(Read::Empty, |(text, _)| Read::Text(text)),
    })
}

/// Empties memory, and the desktop's clipboard when that is where text goes.
async fn clear(agent: &Agent) -> Result<(), String> {
    memory(agent, Memory::clear);
    if place() == Place::System {
        match system::clear().await {
            Ok(()) | Err(Fail::Unavailable(_)) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

/// As `pgpu clip status` shows it. The desktop's clipboard isn't read just for this, so
/// `has_text` only speaks for PocketGPU's own.
async fn status(agent: &Agent) -> ClipStatus {
    let place = current(agent).await;
    let clears_in = memory(agent, |m| m.clears_in(Instant::now()));
    ClipStatus {
        enabled: enabled(agent),
        place,
        has_text: clears_in.is_some(),
        clears_in_s: clears_in.map(|d| d.as_millis().div_ceil(1000) as u32),
    }
}

/// `pgpu clip on` or `off`, saved in `config.toml`. Off also forgets what memory holds.
fn set_enabled(agent: &Agent, on: bool) -> Result<(), String> {
    {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        let mut next = cfg.clone();
        next.clipboard = on;
        next.save(&agent.paths.config_file())
            .map(|()| *cfg = next)
            .map_err(|e| format!("{e:#}"))?;
    }
    if !on {
        memory(agent, Memory::clear);
    }
    let state = if on { "on" } else { "off" };
    if let Err(e) = audit::record(&agent.db(), "local", Event::ClipAccess, "clipboard", state) {
        tracing::warn!("could not write audit entry: {e}");
    }
    Ok(())
}

/// The command line's clipboard requests: `GetClip`, `SetClip`, `ClipGet`, `ClipPut` and
/// `ClipClear` (`pgpu clip`). They work while the clipboard is off: that only keeps other
/// devices out.
pub async fn local_request(agent: &Arc<Agent>, req: Request) -> Response {
    match req {
        Request::GetClip => {}
        Request::SetClip { on } => {
            if let Err(e) = set_enabled(agent, on) {
                return Response::Error(e);
            }
        }
        Request::ClipGet => {
            return match get(agent).await.map(|found| found.read) {
                Ok(Read::Text(text)) => Response::ClipText(Some(text)),
                Ok(Read::Empty) => Response::ClipText(None),
                Ok(Read::Secret) => Response::Error(
                    "what is on the clipboard is marked secret (by a password manager, or a phone \
                     that sent it as secret), so it isn't read; paste it instead"
                        .into(),
                ),
                Err(e) => Response::Error(e.message),
            };
        }
        Request::ClipPut { text, once } => {
            if let Err(e) = check(&text) {
                return Response::Error(e.message);
            }
            put(agent, text, false, once).await;
        }
        Request::ClipClear => {
            if let Err(e) = clear(agent).await {
                return Response::Error(e);
            }
        }
        _ => return Response::Error("not a clipboard request".into()),
    }
    Response::Clip(status(agent).await)
}
