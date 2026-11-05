//! The system's own power commands, run as the agent's user (never elevated), what the
//! system allows that user, and whether the machine comes back by itself after a restart.

use std::process::Command;

use pocketgpu_proto::msg::control::{ComesBack, PowerKind};

const ALL: [PowerKind; 3] = [PowerKind::Restart, PowerKind::Sleep, PowerKind::ShutDown];

/// Runs the action now; the error says why the system refused.
pub fn run(action: PowerKind) -> Result<(), String> {
    let (program, args) = command(action);
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        format!("{program} exited with {}", out.status)
    } else {
        err.chars().take(200).collect()
    })
}

#[cfg(windows)]
fn command(action: PowerKind) -> (&'static str, &'static [&'static str]) {
    match action {
        // `/f` forces running apps to close instead of waiting on them (or an unsaved-work
        // dialog) forever: without it, `shutdown.exe` still exits 0 having only scheduled the
        // restart, so a blocked one looks to the agent like it ran.
        PowerKind::Restart => ("shutdown", &["/r", "/f", "/t", "0"]),
        PowerKind::ShutDown => ("shutdown", &["/s", "/f", "/t", "0"]),
        // Hibernates instead when hibernation is on: Windows decides.
        PowerKind::Sleep => ("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
    }
}

#[cfg(target_os = "macos")]
fn command(action: PowerKind) -> (&'static str, &'static [&'static str]) {
    match action {
        PowerKind::Restart => (
            "osascript",
            &["-e", "tell application \"System Events\" to restart"],
        ),
        PowerKind::ShutDown => (
            "osascript",
            &["-e", "tell application \"System Events\" to shut down"],
        ),
        PowerKind::Sleep => ("pmset", &["sleepnow"]),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn command(action: PowerKind) -> (&'static str, &'static [&'static str]) {
    match action {
        PowerKind::Restart => ("systemctl", &["reboot"]),
        PowerKind::ShutDown => ("systemctl", &["poweroff"]),
        PowerKind::Sleep => ("systemctl", &["suspend"]),
    }
}

/// What the system lets this user do without a password. On Linux logind answers per
/// action; elsewhere a signed-in user may do all three.
pub fn allowed() -> Vec<PowerKind> {
    // End-to-end tests run nothing for real (`PGPU_FAKE_POWER`), whatever the runner allows.
    if std::env::var_os("PGPU_FAKE_POWER").is_some() {
        return ALL.to_vec();
    }
    #[cfg(target_os = "linux")]
    {
        ALL.into_iter()
            .filter(|a| {
                let method = match a {
                    PowerKind::Restart => "CanReboot",
                    PowerKind::ShutDown => "CanPowerOff",
                    PowerKind::Sleep => "CanSuspend",
                };
                logind_answer(method).is_none_or(|v| v == "yes")
            })
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        ALL.to_vec()
    }
}

/// logind's answer (`yes`, `no`, `challenge`, `na`); `None` when it can't be asked.
#[cfg(target_os = "linux")]
fn logind_answer(method: &str) -> Option<String> {
    let out = Command::new("busctl")
        .args([
            "--system",
            "call",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            method,
        ])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_busctl_string(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// `s "yes"` → `yes`.
pub fn parse_busctl_string(out: &str) -> Option<String> {
    let rest = out.trim().strip_prefix("s ")?;
    Some(rest.trim_matches('"').to_string())
}

/// Whether the agent starts again after a restart without anyone at the machine.
pub fn comes_back() -> ComesBack {
    #[cfg(windows)]
    {
        let out = Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon",
                "/v",
                "AutoAdminLogon",
            ])
            .output();
        match out {
            Ok(o) => auto_sign_in(&String::from_utf8_lossy(&o.stdout)),
            Err(_) => ComesBack::Unknown,
        }
    }
    #[cfg(target_os = "macos")]
    {
        let auto = Command::new("defaults")
            .args([
                "read",
                "/Library/Preferences/com.apple.loginwindow",
                "autoLoginUser",
            ])
            .output()
            .is_ok_and(|o| o.status.success());
        if auto {
            ComesBack::Yes
        } else {
            ComesBack::NoAutoSignIn
        }
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(user) = std::env::var("USER") else {
            return ComesBack::Unknown;
        };
        let out = Command::new("loginctl")
            .args(["show-user", &user, "--property=Linger", "--value"])
            .output();
        match out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()) {
            Ok(v) if v == "yes" => ComesBack::Yes,
            Ok(v) if v == "no" => ComesBack::NoLinger,
            _ => ComesBack::Unknown,
        }
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        ComesBack::Unknown
    }
}

/// From `reg query … /v AutoAdminLogon`.
pub fn auto_sign_in(reg_output: &str) -> ComesBack {
    match crate::desktop_app::reg_value(reg_output, "AutoAdminLogon").as_deref() {
        Some("1") => ComesBack::Yes,
        _ => ComesBack::NoAutoSignIn,
    }
}
