//! A pseudo-terminal for `pgpu run`, so programs see a terminal (progress bars, colours)
//! while their output is also logged. Uses only libc calls present in glibc 2.28.

#![cfg(unix)]

#[cfg(not(target_os = "macos"))]
use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

pub struct Pty {
    pub master: OwnedFd,
    slave: OwnedFd,
}

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

impl Pty {
    /// A new pseudo-terminal pair, sized like `like` when that is a terminal. macOS has no
    /// `ptsname_r`; `openpty` is in its libc.
    #[cfg(target_os = "macos")]
    pub fn open(like: RawFd) -> io::Result<Self> {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty writes two new descriptors we then own; the other arguments may be null.
        unsafe {
            check(libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ))?;
            let pty = Self {
                master: OwnedFd::from_raw_fd(master),
                slave: OwnedFd::from_raw_fd(slave),
            };
            copy_size(like, pty.master.as_raw_fd());
            Ok(pty)
        }
    }

    /// A new pseudo-terminal pair, sized like `like` when that is a terminal.
    #[cfg(not(target_os = "macos"))]
    pub fn open(like: RawFd) -> io::Result<Self> {
        // SAFETY: plain libc calls on file descriptors we own; buffers are sized correctly.
        unsafe {
            let master = check(libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY))?;
            let master = OwnedFd::from_raw_fd(master);
            check(libc::grantpt(master.as_raw_fd()))?;
            check(libc::unlockpt(master.as_raw_fd()))?;
            let mut name = [0 as libc::c_char; 128];
            if libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) != 0 {
                return Err(io::Error::last_os_error());
            }
            let path = CStr::from_ptr(name.as_ptr());
            let slave = check(libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY))?;
            let pty = Self {
                master,
                slave: OwnedFd::from_raw_fd(slave),
            };
            copy_size(like, pty.master.as_raw_fd());
            Ok(pty)
        }
    }

    /// Starts `cmd` in a new session with the pseudo-terminal as its terminal, so the job is
    /// its own process group. Drops our copy of the terminal's other end.
    pub fn spawn(self, mut cmd: Command) -> io::Result<(std::process::Child, File)> {
        let io = |fd: &OwnedFd| fd.try_clone().map(Stdio::from);
        cmd.stdin(io(&self.slave)?)
            .stdout(io(&self.slave)?)
            .stderr(io(&self.slave)?);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        drop(self.slave);
        Ok((child, File::from(self.master)))
    }
}

/// Gives the terminal `to` the window size of `from` (at start and after each resize).
pub fn copy_size(from: RawFd, to: RawFd) {
    // SAFETY: TIOCGWINSZ/TIOCSWINSZ read and write a winsize struct we own.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(from, libc::TIOCGWINSZ, &mut ws) == 0 {
            libc::ioctl(to, libc::TIOCSWINSZ, &ws);
        }
    }
}

/// Puts the terminal into raw mode until dropped, so keys (Ctrl+C too) reach the job as typed.
pub struct RawMode {
    fd: RawFd,
    saved: libc::termios,
}

impl RawMode {
    pub fn enable(fd: RawFd) -> io::Result<Self> {
        // SAFETY: tcgetattr/tcsetattr on a terminal fd with a termios struct we own.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            check(libc::tcgetattr(fd, &mut saved))?;
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            check(libc::tcsetattr(fd, libc::TCSANOW, &raw))?;
            Ok(Self { fd, saved })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restores the settings read in `enable`.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
        }
    }
}

/// Sends `signal` to every process in the job's group.
pub fn signal_group(pgid: u32, signal: libc::c_int) {
    // SAFETY: killpg has no memory-safety preconditions.
    unsafe {
        libc::killpg(pgid as libc::pid_t, signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn programs_see_a_terminal() {
        let pty = Pty::open(0).unwrap();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "test -t 1 && echo tty || echo pipe"]);
        let (mut child, mut master) = pty.spawn(cmd).unwrap();
        // Read before waiting: on macOS a program's exit waits until its terminal output has
        // been read, so waiting first never returns (pgpu run reads all along).
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        while !out.contains(&b'\n') {
            let n = master.read(&mut buf).unwrap();
            assert!(n > 0, "the terminal closed before the answer");
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(String::from_utf8_lossy(&out).trim(), "tty");
        assert!(child.wait().unwrap().success());
    }
}
