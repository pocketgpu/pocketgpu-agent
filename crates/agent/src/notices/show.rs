//! Showing a notice through the system's own notifications (F35): Windows toasts, macOS
//! Notification Center, freedesktop notifications on Linux. Each runs on its own thread, so
//! a slow or missing notification service never holds up the request it describes; failures
//! only reach the log. With no desktop session (a server over SSH) nothing is shown, and the
//! activity log is the record.

use std::sync::Arc;

use super::Notice;
use crate::daemon::state::Agent;

/// Button ids on the pairing notice (none on macOS).
#[cfg(not(target_os = "macos"))]
const REVOKE: &str = "revoke";
#[cfg(not(target_os = "macos"))]
const KEEP: &str = "keep";

pub fn show(agent: &Arc<Agent>, notice: Notice) {
    if !session() {
        tracing::debug!(title = %notice.title, "no desktop session; notice only in the log");
        return;
    }
    let agent = agent.clone();
    let spawned = std::thread::Builder::new()
        .name("notice".into())
        .spawn(move || {
            // The notification libraries unwrap on some bus errors; a lost notice must not
            // take anything else down.
            let shown =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| display(&agent, &notice)));
            match shown {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("could not show a notice: {e:#}"),
                Err(_) => tracing::warn!("showing a notice failed"),
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("could not start the notice thread: {e}");
    }
}

/// Whether a desktop is there to show notices on.
fn session() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            || std::env::var_os("XDG_RUNTIME_DIR")
                .is_some_and(|d| std::path::Path::new(&d).join("bus").exists())
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        true
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn display(agent: &Arc<Agent>, notice: &Notice) -> anyhow::Result<()> {
    let mut n = notify_rust::Notification::new();
    n.summary(&notice.title)
        .body(&notice.body)
        .appname("PocketGPU");
    #[cfg(windows)]
    n.app_id(&windows_app_id());
    #[cfg(target_os = "macos")]
    mac_app();
    // macOS delivers button presses only to an app with a run loop, which the agent isn't,
    // so there the notice has no buttons and says how to revoke instead (F36 brings them).
    #[cfg(not(target_os = "macos"))]
    if notice.revoke.is_some() {
        n.action(REVOKE, "Revoke").action(KEEP, "It's mine");
    }
    let handle = n.show()?;
    #[cfg(not(target_os = "macos"))]
    if let Some((device, name)) = &notice.revoke {
        handle.wait_for_action(|action| {
            if action == REVOKE {
                revoke(agent, device, name);
            }
        });
    }
    #[cfg(target_os = "macos")]
    let _ = (handle, agent);
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn display(_agent: &Arc<Agent>, _notice: &Notice) -> anyhow::Result<()> {
    Ok(())
}

/// The person at the computer pressed Revoke on a pairing notice: that press is the
/// confirmation, so the device goes at once, and a second notice says so.
#[cfg(not(target_os = "macos"))]
fn revoke(agent: &Arc<Agent>, device: &str, name: &str) {
    match crate::daemon::devices::revoke(agent, device) {
        Ok(_) => {
            let n = Notice {
                title: format!("{name} was revoked"),
                body: "It can only come back by pairing again at this computer.".into(),
                revoke: None,
            };
            if let Err(e) = display(agent, &n) {
                tracing::warn!("could not show a notice: {e:#}");
            }
        }
        Err(e) => tracing::warn!("could not revoke from a notice: {e}"),
    }
}

/// Toasts show under the PocketGPU app's name when its Start menu shortcut is there (the
/// installer gives it this app id); without the app, under PowerShell's, so they still show.
#[cfg(windows)]
fn windows_app_id() -> String {
    const APP: &str = "app.pocketgpu";
    const POWERSHELL: &str =
        "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";
    let shortcut = std::env::var_os("APPDATA").map(|d| {
        std::path::Path::new(&d).join("Microsoft\\Windows\\Start Menu\\Programs\\PocketGPU.lnk")
    });
    if shortcut.is_some_and(|p| p.exists()) {
        APP.into()
    } else {
        POWERSHELL.into()
    }
}

/// Notices come from the PocketGPU app when it is installed, else from macOS's default.
#[cfg(target_os = "macos")]
fn mac_app() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if notify_rust::set_application("app.pocketgpu").is_err() {
            tracing::debug!("the PocketGPU app isn't installed; notices use the default app");
        }
    });
}
