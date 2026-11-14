//! A live picture of this screen for other devices (F45, board 59): looking only, never a
//! click or a key. Off until `pgpu screen on` here; only devices with Full control (Shell)
//! may look, because a screen can show anything. While a device looks, it is listed here
//! (This PC, the tray, `pgpu screen`) and anyone at this computer can stop it; the first look
//! from a device shows a notice and goes in the activity log.

pub mod capture;
pub mod encode;
pub mod handler;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use pocketgpu_proto::local::{Request, Response, ScreenStatus, ScreenWatcher};
use pocketgpu_proto::screen::Info;
use pocketgpu_proto::{Error, ErrorCode};

use crate::audit::{self, Event};
use crate::daemon::state::Agent;

/// One `screen_look` entry (and notice) per device this often.
const AUDIT_EVERY: Duration = Duration::from_secs(600);

pub const OFF: &str = "Seeing the screen is off on this machine. Turn it on there: pgpu screen on";

pub fn enabled(agent: &Agent) -> bool {
    agent.config.read().expect("config lock poisoned").screen
}

/// One device looking now.
struct Look {
    device: String,
    name: String,
    display: u32,
    since_ms: i64,
    stop: Arc<AtomicBool>,
}

/// Devices looking now, each with a switch that stops it.
#[derive(Default)]
pub struct Watchers {
    next: AtomicU64,
    looks: Mutex<HashMap<u64, Look>>,
}

/// Takes the device off the list when its look ends, however it ends.
pub struct Watching<'a> {
    watchers: &'a Watchers,
    id: u64,
    pub stop: Arc<AtomicBool>,
}

impl Drop for Watching<'_> {
    fn drop(&mut self) {
        self.watchers.lock().remove(&self.id);
    }
}

impl Watchers {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Look>> {
        self.looks.lock().expect("screen lock poisoned")
    }

    pub fn start(&self, device: String, name: String, display: u32) -> Watching<'_> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        self.lock().insert(
            id,
            Look {
                device,
                name,
                display,
                since_ms: crate::store::db::now_ms(),
                stop: stop.clone(),
            },
        );
        Watching {
            watchers: self,
            id,
            stop,
        }
    }

    /// Whether this device (its key) looks now.
    pub fn has(&self, device: &str) -> bool {
        self.lock().values().any(|l| l.device == device)
    }

    /// Stops this device's looks, or everyone's; how many were stopped.
    pub fn stop(&self, device: Option<&str>) -> usize {
        let looks = self.lock();
        let mut n = 0;
        for l in looks.values() {
            if device.is_none_or(|d| d == l.device || l.name == d) {
                l.stop.store(true, Ordering::Relaxed);
                n += 1;
            }
        }
        n
    }

    /// One row per device, oldest look first.
    pub fn list(&self) -> Vec<ScreenWatcher> {
        let mut out: Vec<ScreenWatcher> = Vec::new();
        let mut looks: Vec<_> = self
            .lock()
            .values()
            .map(|l| (l.since_ms, l.device.clone(), l.name.clone(), l.display))
            .collect();
        looks.sort();
        for (since_ms, device, name, display) in looks {
            if !out.iter().any(|w| w.device == device) {
                out.push(ScreenWatcher {
                    device,
                    name,
                    display,
                    since_ms,
                });
            }
        }
        out
    }
}

/// What a device asking `Info` learns: nothing about the displays while it's off.
pub fn info(agent: &Agent) -> Info {
    if !enabled(agent) {
        return Info {
            enabled: false,
            displays: Vec::new(),
            why: None,
        };
    }
    match capture::displays() {
        Ok(displays) => Info {
            enabled: true,
            displays,
            why: None,
        },
        Err(why) => Info {
            enabled: true,
            displays: Vec::new(),
            why: Some(why),
        },
    }
}

/// Refused while off.
pub fn check(agent: &Agent) -> Result<(), Error> {
    if enabled(agent) {
        Ok(())
    } else {
        Err(Error::new(ErrorCode::Forbidden, OFF))
    }
}

/// When each device last had a `screen_look` entry.
static LOOKED: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);

/// Writes `screen_look` (which shows the notice) at most once per device every
/// [`AUDIT_EVERY`], so the small picture on a phone's page doesn't show one each time.
pub fn record_look(agent: &Agent, device: &str, display: u32) {
    {
        let now = Instant::now();
        let mut seen = LOOKED.lock().expect("screen lock poisoned");
        if seen.get(device).is_some_and(|t| now - *t < AUDIT_EVERY) {
            return;
        }
        seen.insert(device.to_string(), now);
    }
    let target = format!("Display {}", display + 1);
    if let Err(e) = audit::record(&agent.db(), device, Event::ScreenLook, &target, "ok") {
        tracing::warn!("could not write audit entry: {e}");
    }
}

fn status(agent: &Agent) -> ScreenStatus {
    ScreenStatus {
        enabled: enabled(agent),
        why: capture::displays().err(),
        watching: agent.screen.list(),
    }
}

fn set(agent: &Agent, on: bool) -> Result<(), String> {
    {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        let mut next = cfg.clone();
        next.screen = on;
        next.save(&agent.paths.config_file())
            .map(|()| *cfg = next)
            .map_err(|e| format!("{e:#}"))?;
    }
    if on {
        capture::ask_leave();
    } else {
        agent.screen.stop(None);
    }
    let what = if on { "on" } else { "off" };
    if let Err(e) = audit::record(&agent.db(), "local", Event::ScreenAccess, "screen", what) {
        tracing::warn!("could not write audit entry: {e}");
    }
    Ok(())
}

/// The command line's and this computer's app's requests (`pgpu screen`).
pub async fn local_request(agent: &Arc<Agent>, req: Request) -> Response {
    let done = match req {
        Request::GetScreen => Ok(()),
        Request::SetScreen { on } => set(agent, on),
        Request::StopScreen { device } => {
            // The log names the device, not its key.
            let name = device.as_deref().map(|d| {
                agent
                    .screen
                    .list()
                    .into_iter()
                    .find(|w| w.device == d || w.name == d)
                    .map_or_else(|| d.to_string(), |w| w.name)
            });
            let n = agent.screen.stop(device.as_deref());
            if n == 0 && device.is_some() {
                Err("That device isn't looking at the screen now.".to_string())
            } else {
                let target = name.unwrap_or_else(|| "everyone".into());
                let _ = audit::record(
                    &agent.db(),
                    "local",
                    Event::ScreenAccess,
                    &target,
                    "stopped",
                );
                Ok(())
            }
        }
        _ => Err("not a screen request".into()),
    };
    match done {
        Ok(()) => {
            let agent = agent.clone();
            match tokio::task::spawn_blocking(move || status(&agent)).await {
                Ok(s) => Response::Screen(s),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Err(e) => Response::Error(e),
    }
}
