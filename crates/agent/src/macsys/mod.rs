//! macOS pieces of the agent (F26): process details without `/proc`, the launchd login
//! agent, and Apple silicon GPU stats (in `collect::apple`).

use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

/// Owner uid, command line and state of one process.
pub struct ProcessInfo {
    pub uid: Option<u32>,
    pub command: String,
    pub alive: bool,
}

pub fn process_info(pid: u32) -> Option<ProcessInfo> {
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing()
            .with_user(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    let p = sys.process(pid)?;
    let mut command = p
        .cmd()
        .iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    if command.is_empty() {
        command = p.name().to_string_lossy().into_owned();
    }
    Some(ProcessInfo {
        uid: p.user_id().map(|u| **u),
        command,
        alive: !matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead),
    })
}

/// The program's name, for the terminal's title.
pub fn process_name(pid: u32) -> Option<String> {
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing(),
    );
    Some(sys.process(pid)?.name().to_string_lossy().into_owned())
}
