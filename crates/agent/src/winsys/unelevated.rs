//! Started as Administrator, `pgpu` runs itself again without admin rights, in the same
//! console: the Administrators group is off (deny-only), every privilege but bypass-traverse is
//! gone and the integrity level is Medium, as in a normal window. The agent, its terminal and
//! its actions then never hold admin rights, also on PCs with UAC turned off, where every
//! program starts elevated. The elevated process only waits and passes the exit code on.

use std::ptr::{null, null_mut};

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_ALL, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION, AddAccessAllowedAce, CreateRestrictedToken, CreateWellKnownSid,
    DISABLE_MAX_PRIVILEGE, GetLengthSid, GetTokenInformation, InitializeAcl, PSID,
    SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ADJUST_DEFAULT,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_OWNER,
    TOKEN_QUERY, TOKEN_USER, TokenDefaultDacl, TokenIntegrityLevel, TokenOwner, TokenUser,
    WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid, WinLocalSystemSid, WinMediumLabelSid,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW, WaitForSingleObject,
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
    // SAFETY: called from `main` before the runtime or any other thread exists.
    unsafe { std::env::set_var(MARKER, "1") };

    // SAFETY: plain Win32 calls on handles and buffers owned here; each result is checked.
    unsafe {
        let mut cmdline: Vec<u16> = {
            let p = GetCommandLineW();
            let len = (0..).take_while(|&i| *p.add(i) != 0).count();
            std::slice::from_raw_parts(p, len).to_vec()
        };
        cmdline.push(0);

        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
        si.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
        si.hStdError = GetStdHandle(STD_ERROR_HANDLE);
        for h in [si.hStdInput, si.hStdOutput, si.hStdError] {
            if !h.is_null() {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
            }
        }
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        // A restricted copy of our own token needs no extra privilege to start a process.
        let ok = CreateProcessAsUserW(
            token.0,
            null(),
            cmdline.as_mut_ptr(),
            null(),
            null(),
            1,
            0,
            null(),
            null(),
            &si,
            &mut pi,
        );
        if ok == 0 {
            bail!(
                "could not start without admin rights: {}",
                std::io::Error::last_os_error()
            );
        }
        let process = Handle(pi.hProcess);
        drop(Handle(pi.hThread));
        // Ctrl+C is for the child, which shares this console; only wait for it. Set after
        // starting it, because the child would inherit the setting.
        SetConsoleCtrlHandler(None, 1);
        WaitForSingleObject(process.0, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(process.0, &mut code);
        Ok(code)
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

/// Our token without admin rights: Administrators deny-only, no privileges, Medium integrity,
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
        let mut restricted: HANDLE = null_mut();
        if CreateRestrictedToken(
            own.0,
            DISABLE_MAX_PRIVILEGE,
            1,
            &deny_admins,
            0,
            null(),
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
