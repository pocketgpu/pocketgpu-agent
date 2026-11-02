//! `pgpu crash-reports [on | off | status]` (board 25): whether the agent sends a report to
//! PocketGPU when it crashes. Off unless turned on, here or when the install script asks. A
//! report holds the error, where in the agent it stopped, its version and the system; never
//! machine names, commands or logs.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{Request, Response};

use crate::ipc::client;
use crate::store::config::Config;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Action {
    /// Show whether crash reports are on (off by default).
    Status,
    /// Send a report when the agent crashes.
    On,
    /// Send nothing.
    Off,
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let set = match action.unwrap_or(Action::Status) {
        Action::Status => None,
        Action::On => Some(true),
        Action::Off => Some(false),
    };
    // The running agent keeps its settings in memory and saves them, so it changes this one
    // too; before it runs (the install script asks first) the file is changed directly.
    let on = match client::call(paths, Request::CrashReports { on: set }).await {
        Ok(Response::CrashReports { on }) => on,
        Ok(Response::Error(e)) => bail!(e),
        Ok(other) => bail!("unexpected reply from the agent: {other:?}"),
        Err(_) => {
            let path = paths.config_file();
            let mut cfg = Config::load(&path)?;
            if let Some(on) = set {
                paths.create_all()?;
                cfg.crash_reports = on;
                cfg.save(&path)?;
            }
            cfg.crash_reports
        }
    };
    if json {
        println!("{}", serde_json::json!({ "crash_reports": on }));
    } else if on {
        println!(
            "Crash reports on: if the agent crashes, it sends the error and where it stopped, its version and the system. Never names, commands or logs. Turn off with: pgpu crash-reports off"
        );
    } else {
        println!("Crash reports off: nothing is sent. Turn on with: pgpu crash-reports on");
    }
    Ok(())
}

/// The status line: `on` or `off`, read from the settings file.
pub fn state(paths: &Paths) -> &'static str {
    match Config::load(&paths.config_file()) {
        Ok(c) if c.crash_reports => "on",
        _ => "off",
    }
}
