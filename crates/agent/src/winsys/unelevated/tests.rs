use std::io::Read;
use std::os::windows::io::{AsRawHandle, FromRawHandle};

use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_PIPE, GetFileType, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    AllocConsole, GetConsoleProcessList, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::GetProcessId;

use super::*;

/// A System32 program's command line, NUL-terminated.
fn system32(line: &str) -> Vec<u16> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\System32\{line}")
        .encode_utf16()
        .chain([0])
        .collect()
}

/// Whether `pid` is on this process's console; `None` with no console here.
fn on_this_console(pid: u32) -> Option<bool> {
    let mut ids = [0u32; 1024];
    // SAFETY: a buffer of the length given.
    let n = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) } as usize;
    (n > 0).then(|| ids[..n.min(ids.len())].contains(&pid))
}

/// This test process's console; one of its own when it has none (a runner without one), so
/// the tests that need it always check something.
fn a_console() {
    if on_this_console(0).is_none() {
        let std = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
        // SAFETY: plain calls; this test's output keeps going where it went.
        unsafe {
            let before = std.map(|which| GetStdHandle(which));
            AllocConsole();
            for (which, h) in std.into_iter().zip(before) {
                SetStdHandle(which, h);
            }
        }
    }
    assert!(on_this_console(0).is_some(), "no console for this test");
}

/// `name` (`CONOUT$`, `NUL`) opened with `access`, as cmd opens `>CON` or `>NUL`.
fn open(name: &str, access: u32) -> Handle {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated name; the handle is the wrapper's from here on.
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    let e = std::io::Error::last_os_error();
    assert_ne!(h, INVALID_HANDLE_VALUE, "{name}: {e}");
    Handle(h)
}

/// A pipe, (read, write).
fn pipe() -> (Handle, Handle) {
    let (mut read, mut write) = (null_mut(), null_mut());
    // SAFETY: out pointers to locals.
    assert_ne!(unsafe { CreatePipe(&mut read, &mut write, null(), 0) }, 0);
    (Handle(read), Handle(write))
}

fn is_pipe(h: HANDLE) -> bool {
    // SAFETY: GetFileType only reads the handle.
    unsafe { GetFileType(h) == FILE_TYPE_PIPE }
}

/// A process on the elevated console could type the Administrator shell's next command.
#[test]
fn the_child_without_admin_rights_never_joins_this_console() {
    a_console();
    let token = restricted_token().unwrap();
    let child = start(
        &token,
        &mut system32("ping.exe -n 6 127.0.0.1"),
        [null_mut(); 3],
        &[],
        None,
    )
    .unwrap();
    // SAFETY: the child's process handle, owned here.
    let pid = unsafe { GetProcessId(child.0) };
    // A process joins its console while it starts; watch for a while.
    let mut joined = false;
    for _ in 0..20 {
        joined |= on_this_console(pid) == Some(true);
        std::thread::sleep(Duration::from_millis(100));
    }
    // SAFETY: as above.
    unsafe { TerminateProcess(child.0, 0) };
    assert!(!joined, "the child is on this console");
}

/// `>CON` in an Administrator cmd opens the console for writing only, which GetConsoleMode
/// doesn't take for one; the child still gets a pipe of ours, never the console itself. NUL
/// goes through a pipe too and still takes everything.
#[test]
fn the_console_reaches_the_child_only_through_a_pipe() {
    a_console();
    let input = open("CONIN$", GENERIC_READ | GENERIC_WRITE);
    for (name, access) in [
        ("CONOUT$", GENERIC_WRITE),
        ("CONOUT$", GENERIC_READ | GENERIC_WRITE),
        ("NUL", GENERIC_WRITE),
    ] {
        let out = open(name, access);
        let wiring = relay::Wiring::of([input.0, out.0, out.0]).unwrap();
        for h in wiring.child {
            assert!(h != input.0 && h != out.0, "{name} reaches the child");
            assert!(is_pipe(h), "{name}: no pipe");
        }
        assert!(!wiring.inherit.iter().any(|h| *h == input.0 || *h == out.0));
    }
}

/// Output to NUL and errors to the console each keep their own place.
#[test]
fn stdout_and_stderr_share_a_pipe_only_on_the_console() {
    a_console();
    let (con, con2, nul) = (
        open("CONOUT$", GENERIC_READ | GENERIC_WRITE),
        open("CONOUT$", GENERIC_READ | GENERIC_WRITE),
        open("NUL", GENERIC_WRITE),
    );
    let both = relay::Wiring::of([null_mut(), con.0, con2.0]).unwrap();
    assert_eq!(both.child[1], both.child[2]);
    let apart = relay::Wiring::of([null_mut(), nul.0, con.0]).unwrap();
    assert_ne!(apart.child[1], apart.child[2]);
    assert!(is_pipe(apart.child[1]) && is_pipe(apart.child[2]));
}

/// `pgpu agent > agent.log`, or into a pipe: the child writes there itself.
#[test]
fn files_and_pipes_reach_the_child_as_they_are() {
    let file = tempfile::tempfile().unwrap();
    let (read, write) = pipe();
    let file_h = file.as_raw_handle() as HANDLE;
    let wiring = relay::Wiring::of([read.0, file_h, write.0]).unwrap();
    assert_eq!(wiring.child, [read.0, file_h, write.0]);
    assert_eq!(wiring.inherit, vec![read.0, file_h, write.0]);
}

/// What the child writes reaches us through the one handle it inherits; in the job it still
/// runs once started.
#[test]
fn the_child_writes_through_the_handles_it_inherits() {
    let token = restricted_token().unwrap();
    let (read, write) = pipe();
    // SAFETY: only the write end's inherit flag changes.
    unsafe { SetHandleInformation(write.0, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    let job = job::Job::new().expect("a job");
    let child = start(
        &token,
        &mut system32("cmd.exe /d /c echo ok"),
        [null_mut(), write.0, write.0],
        &[write.0],
        Some(&job),
    )
    .unwrap();
    drop(write);
    let mut out = String::new();
    let fd = read.0;
    std::mem::forget(read);
    // SAFETY: the read end, owned by the file from here on.
    let mut from = unsafe { std::fs::File::from_raw_handle(fd) };
    from.read_to_string(&mut out).unwrap();
    // SAFETY: the child's process handle, owned here.
    unsafe { WaitForSingleObject(child.0, 10_000) };
    assert_eq!(out.trim(), "ok");
}

/// Killed, or its window closed while a job ignores Ctrl+C, `pgpu` takes the child along:
/// nothing keeps running hidden.
#[test]
fn the_child_ends_with_this_process() {
    let token = restricted_token().unwrap();
    let job = job::Job::new().expect("a job");
    let child = start(
        &token,
        &mut system32("ping.exe -n 30 127.0.0.1"),
        [null_mut(); 3],
        &[],
        Some(&job),
    )
    .unwrap();
    // What this process ending does to the job.
    drop(job);
    // SAFETY: the child's process handle, owned here.
    let ended = unsafe { WaitForSingleObject(child.0, 5_000) } == WAIT_OBJECT_0;
    // SAFETY: as above.
    unsafe { TerminateProcess(child.0, 0) };
    assert!(ended, "the child outlives this process");
}

/// Once the child ended by itself, what it started (an agent in the background) stays.
#[test]
fn what_the_child_leaves_running_stays() {
    let token = restricted_token().unwrap();
    let job = job::Job::new().expect("a job");
    let child = start(
        &token,
        &mut system32("ping.exe -n 30 127.0.0.1"),
        [null_mut(); 3],
        &[],
        Some(&job),
    )
    .unwrap();
    job.let_go();
    drop(job);
    // SAFETY: the child's process handle, owned here.
    let running = unsafe { WaitForSingleObject(child.0, 1_000) } == WAIT_TIMEOUT;
    // SAFETY: as above.
    unsafe { TerminateProcess(child.0, 0) };
    assert!(running, "the child ended with the job");
}
