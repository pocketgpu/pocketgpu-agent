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

/// Stopping or restarting the service (an update, a crash) stops only the agent, as sshd's
/// unit does. Terminals and actions started from phones run in the unit's group, and with the
/// default `control-group` systemd would kill what they started to outlive them, in tmux or
/// with nohup. A plain shell still ends: its terminal hangs up when the agent exits.
const KILL_MODE: &str = concat!(
    "# Only the agent stops: what phones start in tmux or with nohup keeps running.\n",
    "KillMode=process\n",
);

/// The unit file for the binary at `exe`.
pub fn unit_file(exe: &Path) -> String {
    format!(
        "[Unit]
Description=PocketGPU agent
Documentation=https://pocketgpu.app/docs
After=network-online.target
Wants=network-online.target

[Service]
{KILL_MODE}ExecStart=\"{}\" agent
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

/// Whether this process is the service itself: systemd sets `INVOCATION_ID` in what it
/// starts, and the control group names the unit. Terminals and actions started from phones run
/// in the unit's group too, but without `INVOCATION_ID` (`AGENT_ONLY_ENV`), so an agent
/// started by hand, a dev build or a test never counts, even while the service runs.
pub fn is_this_process() -> bool {
    std::env::var_os("INVOCATION_ID").is_some_and(|id| !id.is_empty())
        && std::fs::read_to_string("/proc/self/cgroup").is_ok_and(|c| in_unit(&c))
}

/// Whether `/proc/self/cgroup` puts this process in the unit under the user's service
/// manager: `0::/user.slice/user-1000.slice/user@1000.service/app.slice/pocketgpu.service`
/// (cgroup v2), or that path on the `name=systemd` line (v1).
fn in_unit(cgroup: &str) -> bool {
    cgroup.lines().any(|line| {
        let mut dirs = line.rsplit('/');
        dirs.next() == Some(UNIT) && dirs.any(|d| d.starts_with("user@"))
    })
}

/// Restarts the running service, so it picks up a new binary.
pub fn restart() -> Result<()> {
    systemctl(&["restart", UNIT])
}

/// Once, in the agent that systemd started: a unit written before `KillMode=process` gets
/// it, so the next restart (an update, a crash) leaves what phones started running. An update
/// only swaps the binary, so the unit on disk is still the old agent's. When the file has it
/// but systemd still uses the old kill mode (the reload after writing it failed), it reloads.
pub fn upgrade_unit() {
    if !is_this_process() {
        return;
    }
    let Ok(path) = unit_path() else { return };
    let Ok(old) = std::fs::read_to_string(&path) else {
        return;
    };
    let res = match with_kill_mode(&old) {
        Some(new) => {
            let tmp = path.with_extension("service.new");
            std::fs::write(&tmp, new)
                .and_then(|()| std::fs::rename(&tmp, &path))
                .map_err(anyhow::Error::from)
                .and_then(|()| systemctl(&["daemon-reload"]))
        }
        None if kill_mode_in(&old) == Some("process")
            && loaded_kill_mode().is_some_and(|m| m != "process") =>
        {
            systemctl(&["daemon-reload"])
        }
        None => return,
    };
    match res {
        Ok(()) => tracing::info!("service unit now leaves phones' tmux and nohup jobs running"),
        Err(e) => tracing::warn!("could not update {}: {e:#}", path.display()),
    }
}

/// `unit` with [`KILL_MODE`] at the top of its `[Service]` section, or `None` when it sets a
/// kill mode already (someone chose one) or has no such section.
fn with_kill_mode(unit: &str) -> Option<String> {
    if kill_mode_in(unit).is_some() {
        return None;
    }
    let mut out = String::with_capacity(unit.len() + KILL_MODE.len());
    let mut added = false;
    for line in unit.split_inclusive('\n') {
        out.push_str(line);
        if !added && line.trim() == "[Service]" {
            if !line.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(KILL_MODE);
            added = true;
        }
    }
    added.then_some(out)
}

/// The kill mode a unit file sets, if any; as in systemd, spaces around `=` don't count and
/// the last one wins.
fn kill_mode_in(unit: &str) -> Option<&str> {
    unit.lines()
        .filter_map(|l| l.split_once('='))
        .rfind(|(key, _)| key.trim() == "KillMode")
        .map(|(_, mode)| mode.trim())
}

/// The kill mode systemd uses for the unit, which changes only on `daemon-reload`.
fn loaded_kill_mode() -> Option<String> {
    let out = user_systemctl()
        .args(["show", "-p", "KillMode", "--value", UNIT])
        .output()
        .ok()?;
    let mode = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !mode.is_empty()).then_some(mode)
}

/// Whether stopping the running service leaves what phones started (`KillMode=process`).
pub fn leaves_phone_programs() -> bool {
    state() == "active" && loaded_kill_mode().as_deref() == Some("process")
}

/// What `pgpu service uninstall` says when [`leaves_phone_programs`] held.
pub const LEFT_RUNNING: &str = "  Programs started from phones (tmux, nohup, Ollama) keep running.";

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

#[cfg(test)]
mod tests {
    use super::{in_unit, kill_mode_in, unit_file, with_kill_mode};
    use std::path::Path;

    /// The unit agents wrote before `KillMode=process`.
    const OLD_UNIT: &str = "[Unit]
Description=PocketGPU agent
Documentation=https://pocketgpu.app/docs
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=\"/home/ana/.local/bin/pocketgpu\" agent
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
";

    #[test]
    fn restarts_stop_only_the_agent() {
        // The default, control-group, kills tmux and nohup jobs started from phones.
        let new = unit_file(Path::new("/home/ana/.local/bin/pocketgpu"));
        let service = new.split("[Service]").nth(1).unwrap();
        let service = service.split("\n[").next().unwrap();
        assert!(service.lines().any(|l| l == "KillMode=process"), "{new}");
    }

    #[test]
    fn older_units_get_the_kill_mode_once() {
        let new = unit_file(Path::new("/home/ana/.local/bin/pocketgpu"));
        assert_eq!(with_kill_mode(OLD_UNIT).as_deref(), Some(new.as_str()));
        assert_eq!(with_kill_mode(&new), None);
        // A kill mode someone chose stays; other lines they added stay too.
        let chosen = OLD_UNIT.replace("RestartSec=5\n", "RestartSec=5\nKillMode=mixed\n");
        assert_eq!(with_kill_mode(&chosen), None);
        let edited = OLD_UNIT.replace(
            "RestartSec=5\n",
            "RestartSec=5\nEnvironment=CUDA_VISIBLE_DEVICES=1\n",
        );
        let up = with_kill_mode(&edited).unwrap();
        assert!(up.contains("Environment=CUDA_VISIBLE_DEVICES=1\n"), "{up}");
        assert!(up.contains("[Service]\n# Only the agent stops"), "{up}");
        assert_eq!(with_kill_mode("[Unit]\nDescription=x\n"), None);
        assert_eq!(with_kill_mode(""), None);
    }

    #[test]
    fn reads_the_kill_mode_the_file_sets() {
        // What decides whether a failed reload is tried again.
        let new = unit_file(Path::new("/home/ana/.local/bin/pocketgpu"));
        assert_eq!(kill_mode_in(&new), Some("process"));
        assert_eq!(kill_mode_in(OLD_UNIT), None);
        let last = new.replace("RestartSec=5\n", "RestartSec=5\n  KillMode = mixed\n");
        assert_eq!(kill_mode_in(&last), Some("mixed"), "the last one wins");
        let off = OLD_UNIT.replace("RestartSec=5\n", "RestartSec=5\n# KillMode=process\n");
        assert_eq!(kill_mode_in(&off), None, "a comment sets nothing");
        // A kill mode written with spaces is someone's choice too.
        assert_eq!(with_kill_mode(&off.replace("# ", "")), None);
        assert_eq!(with_kill_mode(&last), None);
    }

    #[test]
    fn only_the_user_unit_is_the_service() {
        // cgroup v2 with and without app.slice, and v1's named hierarchy.
        assert!(in_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/pocketgpu.service\n"
        ));
        assert!(in_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/pocketgpu.service\n"
        ));
        assert!(in_unit(
            "4:memory:/user.slice/user-1000.slice/user@1000.service\n\
             1:name=systemd:/user.slice/user-1000.slice/user@1000.service/pocketgpu.service\n"
        ));
        // A desktop terminal, an SSH login, a system unit of the same name, a container.
        assert!(!in_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.gnome.Terminal.slice/vte-spawn-1.scope\n"
        ));
        assert!(!in_unit("0::/user.slice/user-1000.slice/session-3.scope\n"));
        assert!(!in_unit("0::/system.slice/pocketgpu.service\n"));
        assert!(!in_unit("0::/\n"));
        assert!(!in_unit(""));
    }
}
