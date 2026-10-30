//! The login entry on Windows: `HKCU\...\Run` starts `pocketgpu agent --background`, which
//! starts the agent with no console window. No Windows service: those run outside the user's
//! session, and the terminal (F16) must open the user's own shell.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use pocketgpu_proto::local::{Request, Response};

use crate::store::paths::Paths;
use crate::winsys;

pub fn check() -> Result<()> {
    Ok(())
}

pub fn install(json: bool) -> Result<&'static str> {
    let exe = installed_exe()?;
    winsys::install_login_entry(&exe)?;
    if !running() {
        start(&exe)?;
    }
    if !json {
        println!("✓ Starts at login, and is running now");
        println!("  Log: {}", Paths::discover()?.agent_log().display());
        println!("  Next: pair a phone with `pgpu pair`");
    }
    Ok(state())
}

pub fn uninstall() -> Result<&'static str> {
    winsys::remove_login_entry()?;
    if running() {
        stop()?;
    }
    Ok(state())
}

/// `active`, `inactive` or `not installed`.
pub fn state() -> &'static str {
    if !winsys::has_login_entry() {
        "not installed"
    } else if running() {
        "active"
    } else {
        "inactive"
    }
}

/// Stops the running agent and starts it again, so it picks up a new binary or relays.
pub fn restart() -> Result<()> {
    stop()?;
    start(&installed_exe()?)
}

/// The binary to start. After `pgpu update` this process runs from `pocketgpu.old.exe`.
fn installed_exe() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("could not find the pocketgpu binary")?;
    match exe.file_name().and_then(|n| n.to_str()) {
        Some(n) if n.eq_ignore_ascii_case("pocketgpu.old.exe") => {
            Ok(exe.with_file_name("pocketgpu.exe"))
        }
        _ => Ok(exe),
    }
}

/// Starts the agent in the background and waits until its pipe answers.
pub fn start(exe: &std::path::Path) -> Result<()> {
    let paths = Paths::discover()?;
    paths.create_all()?;
    crate::store::paths::rotate_log(&paths.agent_log());
    winsys::spawn_background(exe, &paths.agent_log())?;
    if wait_for(running, Duration::from_secs(10)) {
        Ok(())
    } else {
        bail!(
            "the agent did not start; see {}",
            paths.agent_log().display()
        )
    }
}

fn stop() -> Result<()> {
    let paths = Paths::discover()?;
    // The command line runs on tokio's multi-threaded runtime.
    let reply = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(crate::ipc::client::call(&paths, Request::Shutdown))
    })?;
    if !matches!(reply, Response::Ok) {
        bail!("the agent did not stop: {reply:?}");
    }
    if wait_for(|| !running(), Duration::from_secs(15)) {
        Ok(())
    } else {
        bail!("the agent is still running after 15 seconds")
    }
}

/// Whether an agent serves the local pipe. A busy pipe has an agent behind it too.
fn running() -> bool {
    const ERROR_PIPE_BUSY: i32 = 231;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(crate::ipc::client::pipe_name())
    {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(ERROR_PIPE_BUSY),
    }
}

fn wait_for(done: impl Fn() -> bool, limit: Duration) -> bool {
    let until = std::time::Instant::now() + limit;
    while std::time::Instant::now() < until {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    done()
}
