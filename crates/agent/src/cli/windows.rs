//! Commands that only do something on Windows, where there is no service manager: the
//! login entry's background start, restarting and watching the agent, and dropping admin
//! rights. Elsewhere they say what to use instead.

#[cfg(windows)]
use std::process::ExitCode;

#[cfg(windows)]
use super::Cli;

#[cfg(windows)]
pub(super) fn background() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    super::service::start_background(&exe)
}

#[cfg(windows)]
pub(super) fn restart_agent() -> anyhow::Result<()> {
    super::service::start_when_stopped()
}

#[cfg(not(windows))]
pub(super) fn restart_agent() -> anyhow::Result<()> {
    anyhow::bail!("restart-agent is for Windows; use pgpu service install")
}

#[cfg(windows)]
pub(super) fn watch_agent() -> anyhow::Result<()> {
    super::service::watch_agent()
}

#[cfg(not(windows))]
pub(super) fn watch_agent() -> anyhow::Result<()> {
    anyhow::bail!("watch-agent is for Windows; use pgpu service install")
}

#[cfg(not(windows))]
pub(super) fn background() -> anyhow::Result<()> {
    anyhow::bail!("--background is for Windows; on Linux and macOS, use `pgpu service install`")
}

/// Started as Administrator: runs the same command again without admin rights (see
/// `winsys::unelevated`). `None` when this process may go on itself.
#[cfg(windows)]
pub(super) fn drop_admin(cli: &Cli) -> Option<ExitCode> {
    use crate::winsys::unelevated;
    if unelevated::is_child() {
        // Ctrl+C at the Administrator console still reaches this one.
        unelevated::follow_parent();
        return None;
    }
    // A service account stays one with fewer rights; the root check then refuses it.
    if !crate::winsys::is_elevated() || crate::winsys::is_service_account() {
        return None;
    }
    // Said once where it matters; `pgpu doctor` shows it too.
    if !cli.json
        && matches!(
            cli.command,
            Some(super::Command::Agent { background: false })
        )
    {
        eprintln!("Started as Administrator; running without admin rights.");
    }
    match unelevated::run_unelevated() {
        Ok(code) => Some(ExitCode::from(u8::try_from(code).unwrap_or(1))),
        // root::check then refuses, as before.
        Err(e) => {
            eprintln!("warning: {e:#}");
            None
        }
    }
}
