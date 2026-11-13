//! The machine's web apps for other devices (F42, board 55): Jupyter, TensorBoard, ComfyUI,
//! Ollama and anything added with `pgpu apps add`. Off until `pgpu apps on` here. Phones and
//! laptops with Shell open them through `pgpu/apps/1`; each of the browser's connections is
//! one stream that ends at the app on this machine's `localhost`. Nothing listens on the
//! network for this: the apps stay where they are, and only paired devices reach them.

pub mod find;
pub mod handler;
pub mod jupyter;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use pocketgpu_proto::apps::{Apps, NEVER};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::{Error, ErrorCode};

use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::config::{AddedApp, Config};

/// One `app_open` audit entry (and notice) per device and app this often.
const AUDIT_EVERY: Duration = Duration::from_secs(600);

fn config(agent: &Agent) -> Config {
    agent.config.read().expect("config lock poisoned").clone()
}

pub fn enabled(agent: &Agent) -> bool {
    agent.config.read().expect("config lock poisoned").apps
}

/// Everything found, for this machine's own command line and app.
pub async fn status(agent: &Agent) -> Apps {
    let cfg = config(agent);
    Apps {
        enabled: cfg.apps,
        apps: find::find(&cfg.apps_added, &cfg.apps_off).await,
    }
}

/// What other devices see: nothing while apps are off.
pub async fn list(agent: &Agent) -> Apps {
    if !enabled(agent) {
        return Apps {
            enabled: false,
            apps: Vec::new(),
        };
    }
    status(agent).await
}

/// Whether other devices may open the app on `port`: apps are on, the port is looked at
/// and not turned off. The app's name comes back for the audit log.
pub fn check(agent: &Agent, port: u16) -> Result<(), Error> {
    let cfg = config(agent);
    if !cfg.apps {
        return Err(Error::new(
            ErrorCode::Forbidden,
            "Apps are off on this machine. Turn them on there with: pgpu apps on",
        ));
    }
    if !find::ports(&cfg.apps_added).contains(&port) {
        return Err(Error::new(
            ErrorCode::Forbidden,
            format!(
                "port {port} isn't one PocketGPU opens. Add it at the machine: pgpu apps add {port}"
            ),
        ));
    }
    if cfg.apps_off.contains(&port) {
        return Err(Error::new(
            ErrorCode::Forbidden,
            "This app is turned off for other devices on this machine.",
        ));
    }
    Ok(())
}

/// When each device last had an `app_open` entry for each app.
static OPENED: LazyLock<Mutex<HashMap<(String, u16), Instant>>> = LazyLock::new(Default::default);

/// Whether `app_open` (which shows the notice) is due for this device and app: at most once
/// per [`AUDIT_EVERY`], because a browser opens many connections for one page. Marks it done.
fn open_due(device: &str, port: u16) -> bool {
    let now = Instant::now();
    let mut seen = OPENED.lock().expect("apps lock poisoned");
    let key = (device.to_string(), port);
    if seen.get(&key).is_some_and(|t| now - *t < AUDIT_EVERY) {
        return false;
    }
    seen.insert(key, now);
    true
}

/// Writes `app_open` when it is due, in the background: finding the app's name asks it for
/// its page, and the browser's connection shouldn't wait for that.
pub fn record_open(agent: &Arc<Agent>, device: String, port: u16) {
    if !open_due(&device, port) {
        return;
    }
    let agent = agent.clone();
    tokio::spawn(async move {
        let cfg = config(&agent);
        let given = cfg.apps_added.iter().find(|a| a.port == port);
        let page = find::page(port).await.unwrap_or_default();
        let name = find::describe(port, &page, given, false).name;
        write_open(&agent, &device, port, &name);
    });
}

fn write_open(agent: &Agent, device: &str, port: u16, name: &str) {
    let target = format!("{name} :{port}");
    if let Err(e) = audit::record(&agent.db(), device, Event::AppOpen, &target, "ok") {
        tracing::warn!("could not write audit entry: {e}");
    }
}

/// Saves a change to the apps settings and audits it.
fn change(agent: &Agent, what: &str, f: impl FnOnce(&mut Config)) -> Result<(), String> {
    {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        let mut next = cfg.clone();
        f(&mut next);
        next.save(&agent.paths.config_file())
            .map(|()| *cfg = next)
            .map_err(|e| format!("{e:#}"))?;
    }
    if let Err(e) = audit::record(&agent.db(), "local", Event::AppsAccess, "apps", what) {
        tracing::warn!("could not write audit entry: {e}");
    }
    Ok(())
}

fn port_ok(port: u16) -> Result<(), String> {
    if port == 0 || NEVER.contains(&port) {
        return Err(format!("port {port} can't be opened from other devices"));
    }
    Ok(())
}

/// The command line's and this computer's app's requests (`pgpu apps`). They work while
/// apps are off: that only keeps other devices out.
pub async fn local_request(agent: &Arc<Agent>, req: Request) -> Response {
    let done = match req {
        Request::GetApps => Ok(()),
        Request::SetApps { on } => change(agent, if on { "on" } else { "off" }, |c| c.apps = on),
        Request::AddApp { port, name } => port_ok(port).and_then(|()| {
            let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
            change(agent, &format!("add {port}"), |c| {
                c.apps_added.retain(|a| a.port != port);
                c.apps_added.push(AddedApp { port, name });
                c.apps_added.sort_by_key(|a| a.port);
            })
        }),
        Request::RemoveApp { port } => {
            if config(agent).apps_added.iter().any(|a| a.port == port) {
                change(agent, &format!("remove {port}"), |c| {
                    c.apps_added.retain(|a| a.port != port);
                    c.apps_off.retain(|p| *p != port);
                })
            } else {
                Err(format!("port {port} wasn't added with: pgpu apps add"))
            }
        }
        Request::SetAppOff { port, off } => {
            let what = format!("{} {port}", if off { "off" } else { "on" });
            change(agent, &what, |c| {
                c.apps_off.retain(|p| *p != port);
                if off {
                    c.apps_off.push(port);
                    c.apps_off.sort_unstable();
                }
            })
        }
        _ => Err("not an apps request".into()),
    };
    match done {
        Ok(()) => Response::Apps(status(agent).await),
        Err(e) => Response::Error(e),
    }
}
