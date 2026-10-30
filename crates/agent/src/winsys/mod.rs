//! Windows pieces of the agent (F21): process trees, owners, elevation, the login entry and
//! the shell. Uses tools every Windows 10 and 11 has (`taskkill`, `reg`), always from
//! System32, and sysinfo, so the agent needs no admin rights.

pub mod unelevated;

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Don't flash a console window for helper commands.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// The per-user login entry: no service, so the agent and its terminal run as the user.
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "PocketGPU";

/// A System32 tool by its full path: a bare name would also be looked up next to the agent's
/// own exe and on PATH.
fn quiet(program: &str) -> Command {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let exe = std::path::Path::new(&root)
        .join("System32")
        .join(format!("{program}.exe"));
    let mut cmd = Command::new(exe);
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Ends a process and everything it started. Without `force`, windows get a close request
/// first; console programs (most jobs) only stop with `force`.
pub fn kill_tree(pid: u32, force: bool) -> Result<()> {
    let mut cmd = quiet("taskkill");
    cmd.args(["/PID", &pid.to_string(), "/T"]);
    if force {
        cmd.arg("/F");
    }
    let out = cmd.output().context("could not run taskkill")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// One piece of this process's token (`TOKEN_*` struct bytes), `None` when it can't be read.
fn token_info(class: windows_sys::Win32::Security::TOKEN_INFORMATION_CLASS) -> Option<Vec<u64>> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: Win32 calls on this process's token, closed below; sizes come from the API.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut len = 0u32;
        GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, class, buf.as_mut_ptr().cast(), len, &mut len) != 0;
        CloseHandle(token);
        ok.then_some(buf)
    }
}

/// Whether this process runs as SYSTEM, LocalService or NetworkService: a token that
/// dropping admin rights can't make an ordinary user.
pub fn is_service_account() -> bool {
    use windows_sys::Win32::Security::{
        IsWellKnownSid, TOKEN_USER, TokenUser, WinLocalServiceSid, WinLocalSystemSid,
        WinNetworkServiceSid,
    };
    let Some(buf) = token_info(TokenUser) else {
        return true;
    };
    // SAFETY: the buffer holds a TOKEN_USER written by GetTokenInformation.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    [WinLocalSystemSid, WinLocalServiceSid, WinNetworkServiceSid]
        .into_iter()
        // SAFETY: a valid SID from the token.
        .any(|kind| unsafe { IsWellKnownSid(sid, kind) } != 0)
}

/// Whether this process runs elevated (as Administrator) or as a service account: the
/// Windows twin of root. High and System integrity count; when the level can't be read, it
/// counts as elevated.
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, TOKEN_MANDATORY_LABEL, TokenIntegrityLevel,
    };
    use windows_sys::Win32::System::SystemServices::SECURITY_MANDATORY_HIGH_RID;
    if is_service_account() {
        return true;
    }
    let Some(buf) = token_info(TokenIntegrityLevel) else {
        return true;
    };
    // SAFETY: the buffer holds a TOKEN_MANDATORY_LABEL written by GetTokenInformation.
    unsafe {
        let sid = (*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return true;
        }
        *GetSidSubAuthority(sid, u32::from(count) - 1) >= SECURITY_MANDATORY_HIGH_RID as u32
    }
}

/// Owner, user name and command line of a process.
pub struct ProcessInfo {
    pub owner: Option<String>,
    pub user: Option<String>,
    pub command: String,
}

pub fn process_info(pid: u32) -> Option<ProcessInfo> {
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    let kind = ProcessRefreshKind::nothing()
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
    let p = sys.process(pid)?;
    let owner = p.user_id().map(|u| u.to_string());
    let user = p.user_id().and_then(|u| {
        let users = sysinfo::Users::new_with_refreshed_list();
        users.get_user_by_id(u).map(|u| u.name().to_string())
    });
    let cmd: Vec<String> = p
        .cmd()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let command = if cmd.is_empty() {
        p.name().to_string_lossy().into_owned()
    } else {
        cmd.join(" ")
    };
    Some(ProcessInfo {
        owner,
        user,
        command,
    })
}

pub fn alive(pid: u32) -> bool {
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid).is_some()
}

/// `pid`, its parent, its parent's parent and so on.
pub fn ancestors(pid: u32) -> Vec<u32> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let mut chain = vec![pid];
    let mut at = Pid::from_u32(pid);
    while let Some(parent) = sys.process(at).and_then(|p| p.parent()) {
        // PIDs are reused, so a cycle is possible in principle.
        if chain.contains(&parent.as_u32()) || chain.len() > 64 {
            break;
        }
        chain.push(parent.as_u32());
        at = parent;
    }
    chain
}

/// PowerShell 7 when installed, else Windows PowerShell.
pub fn default_shell() -> String {
    let on_path = |exe: &str| {
        std::env::var_os("PATH")
            .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(exe).is_file()))
    };
    if on_path("pwsh.exe") {
        "pwsh.exe".into()
    } else {
        "powershell.exe".into()
    }
}

/// Starts `pocketgpu agent` at login for this user.
pub fn install_login_entry(exe: &std::path::Path) -> Result<()> {
    let value = format!("\"{}\" agent --background", exe.display());
    let out = quiet("reg")
        .args([
            "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f",
        ])
        .output()
        .context("could not run reg")?;
    if !out.status.success() {
        bail!(
            "could not add the login entry: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn remove_login_entry() -> Result<()> {
    let _ = quiet("reg")
        .args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
        .output();
    Ok(())
}

pub fn has_login_entry() -> bool {
    quiet("reg")
        .args(["query", RUN_KEY, "/v", RUN_VALUE])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Starts `pocketgpu agent` with no console window, logging to `log`.
pub fn spawn_background(exe: &std::path::Path, log: &std::path::Path) -> Result<()> {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let err = out.try_clone()?;
    keep_std_handles_private();
    Command::new(exe)
        .arg("agent")
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .context("could not start the agent")?;
    Ok(())
}

/// Free bytes for this user on the drive holding `path`, and the drive's root (`C:\\`).
pub fn free_space(path: &std::path::Path) -> Option<(u64, String)> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: a NUL-terminated path and a valid out pointer; the other outputs are optional.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    let root = path.components().take(2).collect::<std::path::PathBuf>();
    // `\\?\C:\` (a long-path prefix) reads as `C:\`.
    let root = root.display().to_string();
    let root = root.strip_prefix(r"\\?\").unwrap_or(&root).to_string();
    (ok != 0).then_some((free, root))
}

/// Windows hands every inheritable handle to a new process. When our output goes to a pipe
/// (`pgpu service install | ...` in PowerShell, or the tray app reading it), the long-running
/// agent would hold that pipe open, and whoever reads it would wait forever.
fn keep_std_handles_private() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: our own standard handles; only their inherit flag changes.
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h as isize != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}
