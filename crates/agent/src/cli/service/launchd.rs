//! The launchd login agent on macOS (F26): the agent starts when the user logs in, runs in
//! their session and never needs root. `~/Library/LaunchAgents/app.pocketgpu.agent.plist`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const LABEL: &str = "app.pocketgpu.agent";

pub fn check() -> Result<()> {
    Ok(())
}

/// The property list for the binary at `exe`, logging to `log`.
pub fn plist(exe: &Path, log: &Path) -> String {
    let esc = |p: &Path| {
        p.display()
            .to_string()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>agent</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>5</integer>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardOutPath</key>
	<string>{}</string>
	<key>StandardErrorPath</key>
	<string>{}</string>
</dict>
</plist>
"#,
        esc(exe),
        esc(log),
        esc(log)
    )
}

fn home() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("could not find a home directory")?
        .home_dir()
        .to_path_buf())
}

fn plist_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

pub fn log_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/Logs/PocketGPU/agent.log"))
}

/// `gui/<uid>`: the user's login session.
fn domain() -> String {
    // SAFETY: getuid has no preconditions and cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

pub fn install(json: bool) -> Result<&'static str> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("could not find the pocketgpu binary")?;
    let (path, log) = (plist_path()?, log_path()?);
    std::fs::create_dir_all(path.parent().expect("plist path has a parent"))?;
    std::fs::create_dir_all(log.parent().expect("log path has a parent"))?;
    std::fs::write(&path, plist(&exe, &log))
        .with_context(|| format!("could not write {}", path.display()))?;
    // A loaded older copy must go first, or bootstrap refuses.
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    launchctl(&["bootstrap", &domain(), &path.to_string_lossy()])?;
    let _ = launchctl(&["enable", &format!("{}/{LABEL}", domain())]);
    if !json {
        println!("✓ Login agent installed and started ({})", path.display());
        println!("  Log: {}", log.display());
        println!("  Next: pair a phone with `pgpu pair`");
    }
    Ok(state())
}

pub fn uninstall() -> Result<&'static str> {
    let path = plist_path()?;
    // Fails harmlessly when it isn't loaded.
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("could not remove {}", path.display()))?;
    }
    Ok(state())
}

/// `active`, `inactive`, `failed` or `not installed`, like the systemd states.
pub fn state() -> &'static str {
    if !plist_path().is_ok_and(|p| p.exists()) {
        return "not installed";
    }
    let out = Command::new("/bin/launchctl")
        .args(["print", &format!("{}/{LABEL}", domain())])
        .output();
    let Ok(out) = out else { return "inactive" };
    let text = String::from_utf8_lossy(&out.stdout);
    if text.lines().any(|l| l.trim() == "state = running") {
        "active"
    } else if text.lines().any(|l| {
        l.trim()
            .strip_prefix("last exit code = ")
            .is_some_and(|c| c != "0" && !c.starts_with("(never"))
    }) {
        "failed"
    } else {
        "inactive"
    }
}

/// Restarts the running agent, so it picks up a new binary.
pub fn restart() -> Result<()> {
    launchctl(&["kickstart", "-k", &format!("{}/{LABEL}", domain())])
}

fn launchctl(args: &[&str]) -> Result<()> {
    let out = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .context("could not run launchctl")?;
    if !out.status.success() {
        bail!(
            "launchctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_runs_the_agent_and_escapes_paths() {
        let p = plist(
            Path::new("/Users/a&b/.local/bin/pocketgpu"),
            Path::new("/tmp/agent.log"),
        );
        assert!(p.contains("<string>/Users/a&amp;b/.local/bin/pocketgpu</string>"));
        assert!(p.contains("<string>agent</string>"));
        assert!(p.contains("<string>app.pocketgpu.agent</string>"));
    }
}
