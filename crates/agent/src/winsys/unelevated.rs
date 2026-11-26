//! Started as Administrator, `pgpu` runs itself again without admin rights: the Administrators
//! group is off (deny-only), only the privileges a standard user has are left and the
//! integrity level is Medium, as in a normal window. The agent, its terminal and its actions
//! then never hold admin rights, also on PCs with UAC turned off, where every program starts
//! elevated. The child runs on a hidden console of its own, never on this one (`relay`); the
//! elevated process only passes keys, output, Ctrl+C and the exit code on, and takes the child
//! along when it ends (`job`).

mod job;
mod relay;

pub use relay::follow_parent;

use std::marker::PhantomData;
use std::ptr::{null, null_mut};
use std::time::Duration;

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION, AddAccessAllowedAce, CreateRestrictedToken, CreateWellKnownSid,
    GetLengthSid, GetTokenInformation, InitializeAcl, LUID_AND_ATTRIBUTES, LookupPrivilegeNameW,
    PSID, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ADJUST_DEFAULT,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_OWNER,
    TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER, TokenDefaultDacl, TokenIntegrityLevel, TokenOwner,
    TokenPrivileges, TokenUser, WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid,
    WinLocalSystemSid, WinMediumLabelSid,
};

/// What a standard user's token holds; every other privilege is deleted. Shutdown is the one
/// that matters: without it `shutdown.exe` and sleep fail with "Access is denied. (5)".
const KEEP: [&str; 5] = [
    "SeShutdownPrivilege",
    "SeChangeNotifyPrivilege",
    "SeUndockPrivilege",
    "SeIncreaseWorkingSetPrivilege",
    "SeTimeZonePrivilege",
];
use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessAsUserW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

/// Set in the process started without admin rights, so it never starts another one.
pub const MARKER: &str = "PGPU_UNELEVATED";

/// Whether this process was started by `run_unelevated`.
pub fn is_child() -> bool {
    std::env::var_os(MARKER).is_some()
}

/// Runs this same command line without admin rights and returns its exit code. Call it first
/// thing in `main`, before any other thread starts.
pub fn run_unelevated() -> Result<u32> {
    let token = restricted_token()?;
    let wiring = relay::Wiring::new()?;
    let ctrl = relay::ctrl_event()?;
    // SAFETY: called from `main` before the runtime or any other thread exists.
    unsafe {
        std::env::set_var(MARKER, "1");
        std::env::set_var(relay::CTRL_VAR, (ctrl as usize).to_string());
    }
    let mut inherit = wiring.inherit.clone();
    inherit.push(ctrl);
    // Before the child exists, so Ctrl+C while it starts never ends this process alone; the
    // event stays set until the child looks.
    relay::forward_ctrl(ctrl);
    let job = job::Job::new();
    let child = start(
        &token,
        &mut own_command_line(),
        wiring.child,
        &inherit,
        job.as_ref(),
    )
    .inspect_err(|_| relay::stop_forwarding())?;
    relay::watch_child(child.0);
    let output = wiring.relay();
    // SAFETY: the child's process handle, owned here.
    unsafe { WaitForSingleObject(child.0, INFINITE) };
    relay::child_ended();
    output.drain(Duration::from_secs(2));
    let mut code = 1u32;
    // SAFETY: as above.
    unsafe { GetExitCodeProcess(child.0, &mut code) };
    if let Some(job) = &job
        && !relay::closing()
    {
        job.let_go();
    }
    Ok(code)
}

/// This process's command line, NUL-terminated.
fn own_command_line() -> Vec<u16> {
    // SAFETY: GetCommandLineW returns this process's command line, NUL-terminated.
    unsafe {
        let p = GetCommandLineW();
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        let mut line = std::slice::from_raw_parts(p, len).to_vec();
        line.push(0);
        line
    }
}

/// Starts `cmdline` (NUL-terminated) with `token` on a console of its own, with no window and
/// `stdio` as its stdin, stdout and stderr. It inherits the handles in `inherit` and no others,
/// and is in `job` before it runs anything (when Windows lets it). Returns its process handle.
fn start(
    token: &Handle,
    cmdline: &mut [u16],
    stdio: [HANDLE; 3],
    inherit: &[HANDLE],
    job: Option<&job::Job>,
) -> Result<Handle> {
    let mut list = match inherit {
        [] => None,
        handles => Some(AttributeList::new(handles)?),
    };
    // SAFETY: Win32 calls on structures owned here; `list` and `inherit` outlive the call.
    unsafe {
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        [
            si.StartupInfo.hStdInput,
            si.StartupInfo.hStdOutput,
            si.StartupInfo.hStdError,
        ] = stdio;
        // Never this console: a process on a console can type into it, and an Administrator
        // shell reads what is typed there once `pgpu` ends.
        let mut flags = CREATE_NO_WINDOW;
        if job.is_some() {
            flags |= CREATE_SUSPENDED;
        }
        if let Some(list) = &mut list {
            si.lpAttributeList = list.ptr();
            flags |= EXTENDED_STARTUPINFO_PRESENT;
        }
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        // A restricted copy of our own token needs no extra privilege to start a process.
        let ok = CreateProcessAsUserW(
            token.0,
            null(),
            cmdline.as_mut_ptr(),
            null(),
            null(),
            i32::from(list.is_some()),
            flags,
            null(),
            null(),
            (&raw const si).cast(),
            &mut pi,
        );
        if ok == 0 {
            bail!(
                "could not start without admin rights: {}",
                std::io::Error::last_os_error()
            );
        }
        let (thread, process) = (Handle(pi.hThread), Handle(pi.hProcess));
        if let Some(job) = job {
            // Refused (a job around this one that can't hold another), it runs as before.
            job.add(process.0);
            if ResumeThread(thread.0) == u32::MAX {
                let e = std::io::Error::last_os_error();
                TerminateProcess(process.0, 1);
                bail!("could not start without admin rights: {e}");
            }
        }
        Ok(process)
    }
}

/// Names the only handles a new process inherits.
struct AttributeList<'a> {
    buf: Vec<u64>,
    handles: PhantomData<&'a [HANDLE]>,
}

impl<'a> AttributeList<'a> {
    fn new(handles: &'a [HANDLE]) -> Result<Self> {
        let fail = |what: &str| anyhow::anyhow!("{what}: {}", std::io::Error::last_os_error());
        let mut size = 0usize;
        // SAFETY: a size query, then a list in a buffer of that size (u64s keep it aligned);
        // the list points at `handles`, which the lifetime keeps alive as long as it.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
            let mut buf = vec![0u64; size.div_ceil(8)];
            if InitializeProcThreadAttributeList(buf.as_mut_ptr().cast(), 1, 0, &mut size) == 0 {
                return Err(fail("InitializeProcThreadAttributeList"));
            }
            let mut list = AttributeList {
                buf,
                handles: PhantomData,
            };
            if UpdateProcThreadAttribute(
                list.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                size_of_val(handles),
                null_mut(),
                null(),
            ) == 0
            {
                return Err(fail("UpdateProcThreadAttribute"));
            }
            Ok(list)
        }
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr().cast()
    }
}

impl Drop for AttributeList<'_> {
    fn drop(&mut self) {
        // SAFETY: a list initialized in `new`, deleted once.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle is owned by this wrapper and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A well-known SID in a buffer of its own.
struct Sid([u8; SECURITY_MAX_SID_SIZE as usize]);

impl Sid {
    fn well_known(kind: WELL_KNOWN_SID_TYPE) -> Result<Self> {
        let mut sid = Sid([0; SECURITY_MAX_SID_SIZE as usize]);
        let mut size = SECURITY_MAX_SID_SIZE;
        // SAFETY: the buffer holds the largest possible SID.
        if unsafe { CreateWellKnownSid(kind, null_mut(), sid.ptr(), &mut size) } == 0 {
            bail!("CreateWellKnownSid: {}", std::io::Error::last_os_error());
        }
        Ok(sid)
    }

    fn ptr(&mut self) -> PSID {
        self.0.as_mut_ptr().cast()
    }
}

/// The privileges in `token` that a standard user doesn't have.
///
/// # Safety
/// `token` must be a token handle opened with `TOKEN_QUERY`.
unsafe fn admin_privileges(token: HANDLE) -> Result<Vec<LUID_AND_ATTRIBUTES>> {
    let mut len = 0u32;
    // SAFETY: a size query, then a read into a buffer of that size (u64s keep it aligned).
    unsafe {
        GetTokenInformation(token, TokenPrivileges, null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if GetTokenInformation(
            token,
            TokenPrivileges,
            buf.as_mut_ptr().cast(),
            len,
            &mut len,
        ) == 0
        {
            bail!("reading privileges: {}", std::io::Error::last_os_error());
        }
        let privs = buf.as_ptr().cast::<TOKEN_PRIVILEGES>();
        let count = (*privs).PrivilegeCount as usize;
        let all = std::slice::from_raw_parts(
            (&raw const (*privs).Privileges).cast::<LUID_AND_ATTRIBUTES>(),
            count,
        );
        Ok(all
            .iter()
            .filter(|p| {
                let mut name = [0u16; 64];
                let mut n = name.len() as u32;
                let named = LookupPrivilegeNameW(null(), &p.Luid, name.as_mut_ptr(), &mut n) != 0;
                !(named && KEEP.contains(&String::from_utf16_lossy(&name[..n as usize]).as_str()))
            })
            .map(|p| LUID_AND_ATTRIBUTES {
                Luid: p.Luid,
                Attributes: 0,
            })
            .collect())
    }
}

/// Our token without admin rights: Administrators deny-only, a standard user's privileges, Medium integrity,
/// and the user (not Administrators) as owner and in the default DACL, so the pipe and files
/// the agent creates belong to the user.
fn restricted_token() -> Result<Handle> {
    let fail = |what: &str| anyhow::anyhow!("{what}: {}", std::io::Error::last_os_error());
    let mut admins = Sid::well_known(WinBuiltinAdministratorsSid)?;
    let mut system = Sid::well_known(WinLocalSystemSid)?;
    let mut medium = Sid::well_known(WinMediumLabelSid)?;

    // SAFETY: Win32 calls on handles and buffers owned here; sizes come from the API.
    unsafe {
        let mut own: HANDLE = null_mut();
        let access = TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT;
        if OpenProcessToken(GetCurrentProcess(), access, &mut own) == 0 {
            return Err(fail("OpenProcessToken"));
        }
        let own = Handle(own);

        // The user's SID, from TOKEN_USER; u64s keep the buffer aligned.
        let mut len = 0u32;
        GetTokenInformation(own.0, TokenUser, null_mut(), 0, &mut len);
        let mut user_buf = vec![0u64; (len as usize).div_ceil(8)];
        if GetTokenInformation(
            own.0,
            TokenUser,
            user_buf.as_mut_ptr().cast(),
            len,
            &mut len,
        ) == 0
        {
            return Err(fail("GetTokenInformation"));
        }
        let user: PSID = (*user_buf.as_ptr().cast::<TOKEN_USER>()).User.Sid;

        let deny_admins = SID_AND_ATTRIBUTES {
            Sid: admins.ptr(),
            Attributes: 0,
        };
        let delete = admin_privileges(own.0)?;
        let mut restricted: HANDLE = null_mut();
        if CreateRestrictedToken(
            own.0,
            0,
            1,
            &deny_admins,
            delete.len() as u32,
            delete.as_ptr(),
            0,
            null(),
            &mut restricted,
        ) == 0
        {
            return Err(fail("CreateRestrictedToken"));
        }
        let restricted = Handle(restricted);

        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: medium.ptr(),
                Attributes: SE_GROUP_INTEGRITY as u32,
            },
        };
        let label_len = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(medium.ptr());
        if SetTokenInformation(
            restricted.0,
            TokenIntegrityLevel,
            (&raw const label).cast(),
            label_len,
        ) == 0
        {
            return Err(fail("setting the integrity level"));
        }

        let owner = TOKEN_OWNER { Owner: user };
        if SetTokenInformation(
            restricted.0,
            TokenOwner,
            (&raw const owner).cast(),
            size_of::<TOKEN_OWNER>() as u32,
        ) == 0
        {
            return Err(fail("setting the owner"));
        }

        // Header plus two ACEs, each at most 8 bytes and a SID.
        let mut acl_buf = vec![0u64; 32];
        let acl: *mut ACL = acl_buf.as_mut_ptr().cast();
        if InitializeAcl(acl, (acl_buf.len() * 8) as u32, ACL_REVISION) == 0
            || AddAccessAllowedAce(acl, ACL_REVISION, GENERIC_ALL, user) == 0
            || AddAccessAllowedAce(acl, ACL_REVISION, GENERIC_ALL, system.ptr()) == 0
        {
            return Err(fail("building the default DACL"));
        }
        let dacl = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
        if SetTokenInformation(
            restricted.0,
            TokenDefaultDacl,
            (&raw const dacl).cast(),
            size_of::<TOKEN_DEFAULT_DACL>() as u32,
        ) == 0
        {
            return Err(fail("setting the default DACL"));
        }
        Ok(restricted)
    }
}

#[cfg(test)]
mod tests;
