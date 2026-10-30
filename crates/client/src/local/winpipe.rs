//! Who is on the other end of the agent's named pipe. The pipe name is predictable and any
//! local user may create a pipe by that name first ("pipe squatting"), so a client checks the
//! server runs as the same Windows user before sending anything, and the agent creates its
//! pipe with a DACL for this user only.

use std::ffi::c_void;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle this wrapper owns, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// The user SID of a process's token, as TOKEN_USER bytes (u64-aligned).
fn token_user(process: HANDLE) -> Option<Vec<u64>> {
    // SAFETY: Win32 calls with handles and buffers owned here; sizes come from the API.
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let token = Owned(token);
        let mut len = 0u32;
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
            return None;
        }
        Some(buf)
    }
}

fn sid_of(buf: &[u64]) -> *mut c_void {
    // SAFETY: the buffer holds a TOKEN_USER written by GetTokenInformation.
    unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid }
}

/// Whether process `pid` runs as the same user as this one.
fn same_user(pid: u32) -> bool {
    // SAFETY: Win32 calls on a process handle owned here.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let process = Owned(process);
        let (Some(theirs), Some(ours)) = (token_user(process.0), token_user(GetCurrentProcess()))
        else {
            return false;
        };
        EqualSid(sid_of(&theirs), sid_of(&ours)) != 0
    }
}

/// The client side: the pipe's server is this user's own process.
pub fn server_is_ours(pipe: &impl AsRawHandle) -> bool {
    let mut pid = 0u32;
    // SAFETY: the borrowed pipe stays open for the call.
    (unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } != 0) && same_user(pid)
}

/// For pipes other programs serve (Docker's): the pipe object belongs to this user, SYSTEM
/// or the Administrators group. Another standard user who created a pipe by that name first
/// owns it, and is refused.
pub fn owner_is_trusted(pipe: &impl AsRawHandle) -> bool {
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        IsWellKnownSid, OWNER_SECURITY_INFORMATION, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    };
    // SAFETY: the borrowed pipe stays open; the descriptor is freed with LocalFree and the
    // owner SID points into it.
    unsafe {
        let mut owner: *mut c_void = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        ) != 0
        {
            return false;
        }
        let trusted = match token_user(GetCurrentProcess()) {
            Some(ours) if EqualSid(owner, sid_of(&ours)) != 0 => true,
            _ => {
                IsWellKnownSid(owner, WinLocalSystemSid) != 0
                    || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
            }
        };
        LocalFree(descriptor);
        trusted
    }
}

/// Security attributes allowing only this user (and SYSTEM) on a new pipe. Keep the value
/// alive while creating the pipe.
pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    /// For `ServerOptions::create_with_security_attributes_raw`; valid while `self` lives.
    pub fn as_raw(&mut self) -> *mut c_void {
        (&raw mut self.attributes).cast()
    }

    pub fn for_current_user() -> Option<Self> {
        // SAFETY: Win32 calls; the SID string and descriptor are freed with LocalFree.
        unsafe {
            let user = token_user(GetCurrentProcess())?;
            let mut sid_str: *mut u16 = null_mut();
            if ConvertSidToStringSidW(sid_of(&user), &mut sid_str) == 0 {
                return None;
            }
            let len = (0..).take_while(|&i| *sid_str.add(i) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
            LocalFree(sid_str.cast());
            // Protected DACL: full access for this user and SYSTEM, nobody else.
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
                .encode_utf16()
                .chain([0])
                .collect();
            let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            ) == 0
            {
                return None;
            }
            Some(Self {
                descriptor,
                attributes: SECURITY_ATTRIBUTES {
                    nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: descriptor,
                    bInheritHandle: 0,
                },
            })
        }
    }
}

// SAFETY: the descriptor is a LocalAlloc block owned only by this value.
unsafe impl Send for PipeSecurity {}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.descriptor) };
    }
}
