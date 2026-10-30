//! `pgpu service install|uninstall|status`: start the agent at login, as the user, never as
//! root or Administrator. A systemd user service on Linux, a launchd login agent on macOS,
//! and a login entry on Windows, which keeps the agent (and its terminal) in the user's own
//! session.

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::root;

#[cfg(target_os = "macos")]
mod launchd;
#[cfg(not(any(windows, target_os = "macos")))]
mod systemd;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use launchd as platform;
#[cfg(target_os = "macos")]
pub use launchd::{log_path, restart, state};
#[cfg(not(any(windows, target_os = "macos")))]
use systemd as platform;
#[cfg(not(any(windows, target_os = "macos")))]
pub use systemd::{UNIT, restart, state, unit_file};
#[cfg(windows)]
use windows as platform;
#[cfg(windows)]
pub use windows::{restart, start as start_background, state};

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Action {
    /// Install and start the service for this user.
    Install,
    /// Stop and remove the service.
    Uninstall,
    /// Show whether the service is installed and running.
    Status,
}

pub fn run(action: Action, json: bool) -> Result<()> {
    platform::check()?;
    root::check()?;
    let state = match action {
        Action::Install => platform::install(json)?,
        Action::Uninstall => platform::uninstall()?,
        Action::Status => state(),
    };
    if json {
        println!("{}", serde_json::json!({ "state": state }));
    } else if !matches!(action, Action::Install) {
        println!("Service {state}");
    }
    Ok(())
}
