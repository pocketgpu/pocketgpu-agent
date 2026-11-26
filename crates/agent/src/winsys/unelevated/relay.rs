//! The elevated `pgpu` and its child without admin rights never share a console. Any process
//! on a console can type into it, so a child there would let anything running as the user (a
//! phone's terminal, say) type the next command into the Administrator shell that started
//! `pgpu`. The child gets a hidden console of its own instead; this process passes keys and
//! output through pipes that only it holds the other ends of, and Ctrl+C through an event the
//! child waits on. Only files and pipes reach the child as they are.

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, GenerateConsoleCtrlEvent, GetConsoleMode,
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForSingleObject,
};
use windows_sys::core::BOOL;

use super::Handle;

/// Names the inherited event that brings Ctrl+C to the child, which removes it at once.
pub const CTRL_VAR: &str = "PGPU_UNELEVATED_CTRL";

/// The child's standard handles: files and pipes passed on as they are, a pipe of ours for each
/// other one (the console above all, also when opened for writing only, as `>CON` does).
pub struct Wiring {
    /// stdin, stdout and stderr, as the child gets them.
    pub child: [HANDLE; 3],
    /// What the child inherits for them, each once.
    pub inherit: Vec<HANDLE>,
    /// The child's ends of our pipes, kept until it has them.
    child_ends: Vec<Handle>,
    /// Keys typed here go into this.
    keys: Option<Handle>,
    /// The child's output comes out of these; true for what goes to our stdout.
    output: Vec<(Handle, bool)>,
}

impl Wiring {
    pub fn new() -> Result<Self> {
        // SAFETY: GetStdHandle only reads this process's standard handles.
        Self::of(
            [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
                .map(|which| unsafe { GetStdHandle(which) }),
        )
    }

    /// For this process's stdin, stdout and stderr.
    pub(super) fn of([input, out, err]: [HANDLE; 3]) -> Result<Self> {
        let mut wiring = Wiring {
            child: [null_mut(); 3],
            inherit: Vec::new(),
            child_ends: Vec::new(),
            keys: None,
            output: Vec::new(),
        };
        wiring.child[0] = match Kind::of(input) {
            Kind::Missing => null_mut(),
            Kind::Pass => pass_on(input),
            Kind::Relay => {
                let (read, write) = pipe()?;
                wiring.keys = Some(write);
                wiring.share(read)?
            }
        };
        wiring.child[1] = wiring.output_for(out, true)?;
        // Both on the console: one pipe keeps their order, as there.
        wiring.child[2] = if out == err || (is_console(out) && is_console(err)) {
            wiring.child[1]
        } else {
            wiring.output_for(err, false)?
        };
        for h in wiring.child {
            if !h.is_null() && !wiring.inherit.contains(&h) {
                wiring.inherit.push(h);
            }
        }
        Ok(wiring)
    }

    /// What the child gets for our stdout or stderr `h`.
    fn output_for(&mut self, h: HANDLE, to_stdout: bool) -> Result<HANDLE> {
        Ok(match Kind::of(h) {
            Kind::Missing => null_mut(),
            Kind::Pass => pass_on(h),
            Kind::Relay => {
                let (read, write) = pipe()?;
                self.output.push((read, to_stdout));
                self.share(write)?
            }
        })
    }

    /// Makes the child's end of a pipe inheritable and keeps it until the child has it.
    fn share(&mut self, end: Handle) -> Result<HANDLE> {
        // SAFETY: a pipe handle owned here; only its inherit flag changes.
        if unsafe { SetHandleInformation(end.0, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            bail!("pipe: {}", std::io::Error::last_os_error());
        }
        let h = end.0;
        self.child_ends.push(end);
        Ok(h)
    }

    /// Once the child has started: closes our copies of its ends, so its output ends with it,
    /// and starts passing keys and output on.
    pub fn relay(mut self) -> Output {
        self.child_ends.clear();
        if let Some(keys) = self.keys.take() {
            let mut to_child = keys.into_file();
            // May stay blocked on the console after the child ends; the process exits anyway.
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let mut stdin = std::io::stdin();
                while let Ok(n) = stdin.read(&mut buf) {
                    if n == 0 || to_child.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
        }
        Output(
            std::mem::take(&mut self.output)
                .into_iter()
                .map(|(from, to_stdout)| pass_output(from, to_stdout))
                .collect(),
        )
    }
}

/// Passes what comes out of `from` on to our stdout or stderr; the receiver hears when it ends.
fn pass_output(from: Handle, to_stdout: bool) -> mpsc::Receiver<()> {
    let mut from = from.into_file();
    let (done, ended) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = Text::default();
        let mut buf = [0u8; 8192];
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 {
                break;
            }
            let shown = text.push(&buf[..n]);
            let _ = if to_stdout {
                show(std::io::stdout(), &shown)
            } else {
                show(std::io::stderr(), &shown)
            };
        }
        let _ = done.send(());
    });
    ended
}

/// The threads passing the child's output on.
pub struct Output(Vec<mpsc::Receiver<()>>);

impl Output {
    /// Waits up to `within` for output still in the pipes: something the child left running
    /// may hold them open for good.
    pub fn drain(self, within: Duration) {
        let until = Instant::now() + within;
        for ended in self.0 {
            let _ = ended.recv_timeout(until.saturating_duration_since(Instant::now()));
        }
    }
}

impl Handle {
    fn into_file(self) -> File {
        let h = self.0;
        std::mem::forget(self);
        // SAFETY: the handle was this wrapper's alone; the file owns it now.
        unsafe { File::from_raw_handle(h) }
    }
}

/// What the child gets for one of our standard handles.
enum Kind {
    /// Nothing: there is nothing to pass on.
    Missing,
    /// A file or a pipe, as it is.
    Pass,
    /// A pipe of ours: for the console (which GetConsoleMode misses when it is open for writing
    /// only), NUL, a COM port, anything Windows can't name.
    Relay,
}

impl Kind {
    fn of(h: HANDLE) -> Kind {
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return Kind::Missing;
        }
        // SAFETY: GetFileType only reads the handle.
        match unsafe { GetFileType(h) } {
            FILE_TYPE_DISK | FILE_TYPE_PIPE => Kind::Pass,
            _ => Kind::Relay,
        }
    }
}

/// A console handle open for reading (for writing only, GetConsoleMode fails on it).
fn is_console(h: HANDLE) -> bool {
    let mut mode = 0;
    // SAFETY: GetConsoleMode only reads the handle and fails on anything not a console.
    !h.is_null() && h != INVALID_HANDLE_VALUE && unsafe { GetConsoleMode(h, &mut mode) } != 0
}

/// A file or pipe the child gets as it is, or none when it can't be inherited.
fn pass_on(h: HANDLE) -> HANDLE {
    // SAFETY: one of our standard handles; only its inherit flag changes.
    if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
        return null_mut();
    }
    h
}

/// An anonymous pipe, (read, write), neither end inheritable. It has this elevated process's
/// default security, so nothing without admin rights can open it.
fn pipe() -> Result<(Handle, Handle)> {
    let (mut read, mut write) = (null_mut(), null_mut());
    // SAFETY: out pointers to locals; no attributes, so neither end is inheritable.
    if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
        bail!("pipe: {}", std::io::Error::last_os_error());
    }
    Ok((Handle(read), Handle(write)))
}

fn show(mut to: impl Write, text: &str) -> std::io::Result<()> {
    to.write_all(text.as_bytes())?;
    to.flush()
}

/// Output for the console: UTF-8 as it is, a character split between reads joined up, and
/// bytes that are no UTF-8 as U+FFFD. Windows refuses to show them, and a refused write would
/// stay stuck in stdout's buffer.
#[derive(Default)]
struct Text {
    rest: Vec<u8>,
}

impl Text {
    fn push(&mut self, bytes: &[u8]) -> String {
        let mut data = std::mem::take(&mut self.rest);
        data.extend_from_slice(bytes);
        let mut text = String::with_capacity(data.len());
        let mut left = &data[..];
        loop {
            match std::str::from_utf8(left) {
                Ok(s) => {
                    text.push_str(s);
                    return text;
                }
                Err(e) => {
                    let (good, bad) = left.split_at(e.valid_up_to());
                    text.push_str(std::str::from_utf8(good).unwrap_or_default());
                    match e.error_len() {
                        Some(n) => {
                            text.push('\u{FFFD}');
                            left = &bad[n..];
                        }
                        // Cut off at the end: the next read has the rest.
                        None => {
                            self.rest = bad.to_vec();
                            return text;
                        }
                    }
                }
            }
        }
    }
}

/// The event the console's handler sets, the child it waits for when the window closes (0 until
/// it exists and once it has ended), and whether the window is closing.
static EVENT: AtomicUsize = AtomicUsize::new(0);
static CHILD: AtomicUsize = AtomicUsize::new(0);
static CLOSING: AtomicBool = AtomicBool::new(false);

/// An event the child inherits and waits on, for Ctrl+C here. It stays open until this process
/// ends.
pub fn ctrl_event() -> Result<HANDLE> {
    let inherit = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: an unnamed auto-reset event, not set.
    let event = unsafe { CreateEventW(&inherit, 0, 0, null()) };
    if event.is_null() {
        bail!("event: {}", std::io::Error::last_os_error());
    }
    Ok(event)
}

/// From now on Ctrl+C, Ctrl+Break and closing the window here reach the child as Ctrl+C, also
/// while it starts. A handler isn't inherited, so the child never has this one.
pub fn forward_ctrl(event: HANDLE) {
    EVENT.store(event as usize, Ordering::SeqCst);
    // SAFETY: the handler only uses the statics above.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
}

/// The child could not start: Ctrl+C ends this process again.
pub fn stop_forwarding() {
    // SAFETY: removes the handler `forward_ctrl` added.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 0) };
}

/// The child to wait for when the window closes; its process handle stays open until
/// `child_ended`.
pub fn watch_child(child: HANDLE) {
    CHILD.store(child as usize, Ordering::SeqCst);
}

/// Before the child's process handle closes.
pub fn child_ended() {
    CHILD.store(0, Ordering::SeqCst);
}

/// Whether the window here is closing: then the child and what it started end with us.
pub fn closing() -> bool {
    CLOSING.load(Ordering::SeqCst)
}

unsafe extern "system" fn on_ctrl(kind: u32) -> BOOL {
    if !matches!(kind, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT) {
        return 0;
    }
    if kind == CTRL_CLOSE_EVENT {
        CLOSING.store(true, Ordering::SeqCst);
    }
    // SAFETY: the event stays open until this process ends.
    unsafe { SetEvent(EVENT.load(Ordering::SeqCst) as HANDLE) };
    let child = CHILD.load(Ordering::SeqCst);
    if kind == CTRL_CLOSE_EVENT && child != 0 {
        // Windows ends this process a few seconds after, and the job with it all that is left;
        // let the child stop first.
        // SAFETY: the child's process handle, open while CHILD holds it.
        unsafe { WaitForSingleObject(child as HANDLE, 4000) };
    }
    1
}

/// In the child: Ctrl+C at the elevated console arrives as the event `CTRL_VAR` names and goes
/// to everything on the child's own console, so the agent and its jobs stop as before. Call it
/// first thing in `main`, before any other thread starts.
pub fn follow_parent() {
    let Some(value) = std::env::var_os(CTRL_VAR) else {
        return;
    };
    // Gone before anything starts, so nothing this process starts takes the handle for its own.
    // SAFETY: called from `main` before the runtime or any other thread exists.
    unsafe { std::env::remove_var(CTRL_VAR) };
    let Some(event) = value.to_str().and_then(|v| v.parse::<usize>().ok()) else {
        return;
    };
    // SAFETY: only the inherit flag changes; what this process starts has no use for it.
    unsafe { SetHandleInformation(event as HANDLE, HANDLE_FLAG_INHERIT, 0) };
    std::thread::spawn(move || {
        // SAFETY: an event this process inherited, open for good.
        while unsafe { WaitForSingleObject(event as HANDLE, INFINITE) } == WAIT_OBJECT_0 {
            // SAFETY: plain call; 0 is every process on this console, the child's own.
            unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ctrl+C before the child exists is kept for it and doesn't end this process; the
    /// handler is on from before the child starts.
    #[test]
    fn ctrl_c_while_the_child_starts_waits_for_it() {
        let event = ctrl_event().unwrap();
        // The handler's statics, set without adding it to this test process.
        EVENT.store(event as usize, Ordering::SeqCst);
        assert_eq!(CHILD.load(Ordering::SeqCst), 0);
        // SAFETY: the handler only uses the statics; the event is open.
        unsafe {
            assert_eq!(on_ctrl(CTRL_C_EVENT), 1);
            assert_eq!(WaitForSingleObject(event, 0), WAIT_OBJECT_0);
        }
        drop(Handle(event));
        assert!(!closing());
    }

    #[test]
    fn output_joins_characters_split_between_reads() {
        let mut text = Text::default();
        let bytes = "▶ Job 3".as_bytes();
        assert_eq!(text.push(&bytes[..2]), "");
        assert_eq!(text.push(&bytes[2..]), "▶ Job 3");
    }

    #[test]
    fn output_shows_bytes_that_are_no_utf8() {
        let mut text = Text::default();
        assert_eq!(text.push(b"caf\xe9 ok\n"), "caf\u{FFFD} ok\n");
        assert_eq!(text.push(b"end \xe2"), "end ");
        assert_eq!(text.push(b"\n"), "\u{FFFD}\n");
    }
}
