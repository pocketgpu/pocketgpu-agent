//! The systemd user service on Linux: the agent starts at login (or at boot with lingering)
//! and never needs root.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const UNIT: &str = "pocketgpu.service";

pub fn check() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("`pgpu service` uses systemd and works on Linux, macOS and Windows only");
    }
    Ok(())
}

/// The unit file for the binary at `exe`.
pub fn unit_file(exe: &Path) -> String {
    format!(
        "[Unit]
Description=PocketGPU agent
Documentation=https://pocketgpu.app/docs
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=\"{}\" agent
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
",
        exe.display()
    )
}

fn unit_path() -> Result<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => directories::BaseDirs::new()
            .context("could not find a home directory")?
            .home_dir()
            .join(".config"),
    };
    Ok(config.join("systemd/user").join(UNIT))
}

pub fn install(json: bool) -> Result<&'static str> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("could not find the pocketgpu binary")?;
    let path = unit_path()?;
    std::fs::create_dir_all(path.parent().expect("unit path has a parent"))?;
    std::fs::write(&path, unit_file(&exe))
        .with_context(|| format!("could not write {}", path.display()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT])?;
    if !json {
        println!("✓ Service installed and started ({})", path.display());
        if linger() == Some(false) {
            let user = std::env::var("USER").unwrap_or_else(|_| "$USER".into());
            println!(
                "  The agent stops when you log out. To keep it running and start it at boot, run:\n  sudo loginctl enable-linger {user}"
            );
        }
        println!("  Next: pair a phone with `pgpu pair`");
    }
    Ok(state())
}

pub fn uninstall() -> Result<&'static str> {
    let path = unit_path()?;
    if path.exists() {
        // Stopping fails harmlessly when it isn't running.
        let _ = systemctl(&["disable", "--now", UNIT]);
        std::fs::remove_file(&path)
            .with_context(|| format!("could not remove {}", path.display()))?;
        systemctl(&["daemon-reload"])?;
    }
    Ok(state())
}

/// `active`, `inactive`, `failed` or `not installed`.
pub fn state() -> &'static str {
    if !unit_path().is_ok_and(|p| p.exists()) {
        return "not installed";
    }
    let out = user_systemctl().args(["is-active", UNIT]).output();
    match out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()) {
        Ok(s) if s == "active" => "active",
        Ok(s) if s == "failed" => "failed",
        _ => "inactive",
    }
}

/// Restarts the running service, so it picks up a new binary.
pub fn restart() -> Result<()> {
    systemctl(&["restart", UNIT])
}

/// `systemctl --user`, reaching the user's service manager even without a login session
/// (after `su - user` or `sudo -u user`, which don't set its address) when it is running,
/// as it is with lingering.
fn user_systemctl() -> Command {
    let mut cmd = Command::new("systemctl");
    cmd.arg("--user");
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        // SAFETY: getuid has no preconditions and cannot fail.
        let run = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
        if run.join("bus").exists() {
            cmd.env("XDG_RUNTIME_DIR", &run).env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", run.join("bus").display()),
            );
        }
    }
    cmd
}

fn systemctl(args: &[&str]) -> Result<()> {
    let out = user_systemctl()
        .args(args)
        .output()
        .context("could not run systemctl. Is systemd running? In a container, use `pgpu agent`")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("Failed to connect to bus") {
            bail!("{}", no_user_manager());
        }
        bail!("systemctl --user {} failed: {}", args.join(" "), err.trim());
    }
    Ok(())
}

/// What to do when the user's service manager can't be reached: the usual cause is a shell
/// opened with `su` or `sudo` as root, which has no login session for this user.
fn no_user_manager() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "$USER".into());
    format!(
        "your user's service manager isn't running (common after `su` or `sudo` from root). \
         As root or with sudo, run `loginctl enable-linger {user}`, then run `pgpu service install` \
         again. It also makes the agent start at boot"
    )
}

/// Whether the user's services outlive their sessions; `None` when it can't be told.
fn linger() -> Option<bool> {
    let user = std::env::var("USER").ok()?;
    let out = Command::new("loginctl")
        .args(["show-user", &user, "--property=Linger", "--value"])
        .output()
        .ok()?;
    match String::from_utf8_lossy(&out.stdout).trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}
