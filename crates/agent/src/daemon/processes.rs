//! GPU processes and kill (F09): who is using each GPU, and stopping your own processes.
//!
//! The agent runs as a normal user, so only that user's processes can be killed. Other
//! users' processes are listed with `killable: false`. On Windows the owner is the account's
//! SID and a kill ends the process tree. Only processes the driver reports on
//! a GPU can be killed; this is not a general `kill` for any PID.

use std::sync::Arc;
use std::time::Duration;

use pocketgpu_proto::msg::control::Proc;
use pocketgpu_proto::{Error, ErrorCode};

use super::state::Agent;
use crate::collect::GpuProcess;

/// Time between SIGTERM and SIGKILL (F09).
pub const KILL_GRACE: Duration = Duration::from_secs(10);
/// Longest command line shown on the phone.
const MAX_COMMAND: usize = 200;

/// GPU processes, largest VRAM first.
pub fn list(agent: &Agent) -> Vec<Proc> {
    let me = current_owner();
    let mut out: Vec<Proc> = agent
        .sampler
        .gpus()
        .processes()
        .into_iter()
        .map(|p| describe(&p, me.as_deref()))
        .collect();
    out.sort_by(|a, b| b.vram_mb.cmp(&a.vram_mb).then(a.pid.cmp(&b.pid)));
    out
}

/// Sends SIGTERM now and SIGKILL after [`KILL_GRACE`] if the process is still there.
/// Returns the process's command for the audit log.
pub fn kill(agent: &Arc<Agent>, pid: u32) -> Result<String, Error> {
    let Some(proc) = list(agent).into_iter().find(|p| p.pid == pid) else {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!("process {pid} is not using a GPU"),
        ));
    };
    if !proc.killable {
        return Err(Error::new(
            ErrorCode::Forbidden,
            format!(
                "process {pid} belongs to {}; the agent can only stop its own user's processes",
                proc.user
            ),
        ));
    }
    let started = start_time(pid);
    signal(pid, Signal::Term)?;
    tokio::spawn(async move {
        tokio::time::sleep(KILL_GRACE).await;
        // The same process, not a new one that got its PID after it ended.
        if alive(pid) && started.is_some() && start_time(pid) == started {
            tracing::info!(pid, "still running after SIGTERM, sending SIGKILL");
            let _ = signal(pid, Signal::Kill);
        }
    });
    Ok(proc.command)
}

fn describe(p: &GpuProcess, me: Option<&str>) -> Proc {
    let owner = owner_of(p.pid);
    let own = owner.is_some() && owner.as_deref() == me;
    Proc {
        pid: p.pid,
        user: owner
            .as_deref()
            .map(user_name)
            .unwrap_or_else(|| "?".into()),
        command: p
            .label
            .clone()
            .or_else(|| command_line(p.pid))
            .unwrap_or_else(|| format!("pid {}", p.pid)),
        gpu: p.gpu,
        vram_mb: p.vram_mb.unwrap_or(0),
        // Never offer to kill the agent itself (the fake GPU reports it as a GPU process).
        killable: own && p.pid != std::process::id(),
    }
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal(pid: u32, sig: Signal) -> Result<(), Error> {
    let sig = match sig {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: kill has no memory-safety preconditions.
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if rc == 0 {
        Ok(())
    } else {
        let e = std::io::Error::last_os_error();
        Err(Error::new(
            ErrorCode::Internal,
            format!("could not stop {pid}: {e}"),
        ))
    }
}

/// Windows: a close request to the process tree first; console programs refuse it, so they
/// are ended at once. `Kill` ends the tree.
#[cfg(windows)]
fn signal(pid: u32, sig: Signal) -> Result<(), Error> {
    let polite = matches!(sig, Signal::Term) && crate::winsys::kill_tree(pid, false).is_ok();
    if polite {
        return Ok(());
    }
    crate::winsys::kill_tree(pid, true)
        .map_err(|e| Error::new(ErrorCode::Internal, format!("could not stop {pid}: {e:#}")))
}

/// When the process started (seconds since 1970), to tell it from a later one with its PID.
fn start_time(pid: u32) -> Option<u64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid).map(|p| p.start_time())
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    crate::winsys::alive(pid)
}

#[cfg(target_os = "macos")]
fn alive(pid: u32) -> bool {
    crate::macsys::process_info(pid).is_some_and(|p| p.alive)
}

#[cfg(target_os = "macos")]
fn owner_of(pid: u32) -> Option<String> {
    crate::macsys::process_info(pid)?.uid.map(|u| u.to_string())
}

#[cfg(target_os = "macos")]
fn command_line(pid: u32) -> Option<String> {
    let mut cmd = crate::macsys::process_info(pid)?.command;
    if cmd.chars().count() > MAX_COMMAND {
        cmd = cmd.chars().take(MAX_COMMAND - 1).collect::<String>() + "…";
    }
    Some(cmd)
}

/// Whether the process still exists and isn't a zombie.
#[cfg(not(any(windows, target_os = "macos")))]
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state follows the command in parentheses, which may itself contain spaces.
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X"),
        Err(_) => false,
    }
}

/// Who owns the process: the uid on Unix, the account's SID on Windows.
#[cfg(not(any(windows, target_os = "macos")))]
fn owner_of(pid: u32) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string)
}

#[cfg(windows)]
fn owner_of(pid: u32) -> Option<String> {
    crate::winsys::process_info(pid)?.owner
}

#[cfg(windows)]
fn command_line(pid: u32) -> Option<String> {
    let mut cmd = crate::winsys::process_info(pid)?.command;
    if cmd.chars().count() > MAX_COMMAND {
        cmd = cmd.chars().take(MAX_COMMAND - 1).collect::<String>() + "…";
    }
    Some(cmd)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn command_line(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let mut cmd = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    if cmd.is_empty() {
        // Kernel threads and some short-lived processes have no command line.
        cmd = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()?
            .trim()
            .to_string();
    }
    if cmd.chars().count() > MAX_COMMAND {
        cmd = cmd.chars().take(MAX_COMMAND - 1).collect::<String>() + "…";
    }
    Some(cmd)
}

#[cfg(unix)]
fn current_owner() -> Option<String> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    Some(unsafe { libc::geteuid() }.to_string())
}

#[cfg(windows)]
fn current_owner() -> Option<String> {
    owner_of(std::process::id())
}

#[cfg(not(any(unix, windows)))]
fn current_owner() -> Option<String> {
    None
}

#[cfg(unix)]
fn user_name(owner: &str) -> String {
    let Ok(uid) = owner.parse::<u32>() else {
        return owner.to_string();
    };
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: an all-zero passwd is a valid value for getpwuid_r to fill.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: the buffers outlive the call and their lengths are passed correctly.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
        return uid.to_string();
    }
    // SAFETY: on success pw_name points to a NUL-terminated string inside `buf`.
    unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
fn user_name(owner: &str) -> String {
    let users = sysinfo::Users::new_with_refreshed_list();
    users
        .iter()
        .find(|u| u.id().to_string() == owner)
        .map(|u| u.name().to_string())
        .unwrap_or_else(|| owner.to_string())
}

#[cfg(not(any(unix, windows)))]
fn user_name(owner: &str) -> String {
    owner.to_string()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn describes_its_own_process() {
        let me = std::process::id();
        let p = describe(
            &GpuProcess {
                pid: me,
                gpu: 0,
                vram_mb: Some(512),
                label: None,
            },
            current_owner().as_deref(),
        );
        assert!(p.command.contains("pocketgpu") || !p.command.is_empty());
        assert_ne!(p.user, "?");
        assert!(!p.killable, "the agent must never offer to kill itself");
        assert!(alive(me));
    }

    #[test]
    fn processes_of_other_users_are_not_killable() {
        // PID 1 belongs to root (or another user in containers); the tests never run as it.
        let p = describe(
            &GpuProcess {
                pid: 1,
                gpu: 0,
                vram_mb: None,
                label: None,
            },
            Some("4294967294"),
        );
        assert!(!p.killable);
    }

    #[test]
    fn a_missing_process_is_not_alive() {
        assert!(!alive(u32::MAX - 7));
    }
}
