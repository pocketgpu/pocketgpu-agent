//! One pseudo-terminal running the user's login shell.
//!
//! Output is read on a thread started by `Reader::start`, so nothing is missed while the
//! caller sets up; input goes through a writer thread so a full PTY buffer never blocks the
//! async runtime.

use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::mpsc;

use anyhow::{Context, Result};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Input chunks waiting for the PTY; each is at most one frame.
const INPUT_QUEUE: usize = 32;

pub struct Pty {
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    input: mpsc::SyncSender<Vec<u8>>,
    shell: String,
}

/// What the output thread reports.
pub enum Output {
    Bytes(Vec<u8>),
    /// The shell ended: its exit code, or `None` when a signal ended it.
    Exited(Option<i32>),
}

impl Pty {
    /// Starts the login shell in the home directory, `cols` × `rows` cells. Its output waits
    /// in the terminal until `Reader::start`.
    pub fn spawn(cols: u16, rows: u16) -> Result<(Self, Reader)> {
        let pair = native_pty_system()
            .openpty(size(cols, rows))
            .context("could not open a terminal")?;
        // The user's login shell, from $SHELL or the password database; PowerShell on Windows
        // (ConPTY).
        #[cfg(windows)]
        let mut cmd = CommandBuilder::new(crate::winsys::default_shell());
        #[cfg(not(windows))]
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for key in crate::AGENT_ONLY_ENV {
            cmd.env_remove(key);
        }
        if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
            cmd.cwd(home);
        }
        #[cfg(windows)]
        let shell = crate::winsys::default_shell();
        #[cfg(not(windows))]
        let shell = cmd.get_shell();
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("could not start the shell")?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;

        // Bounded, so a phone can't queue unlimited input behind a program that stopped reading.
        let (input, inputs) = mpsc::sync_channel::<Vec<u8>>(INPUT_QUEUE);
        std::thread::Builder::new()
            .name("pty-input".into())
            .spawn(move || {
                for bytes in inputs {
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })?;

        let pty = Self {
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            input,
            shell: shell_name(&shell),
        };
        Ok((pty, Reader { reader, child }))
    }

    /// Dropped when the queue is full: the program isn't reading anyway.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.input.try_send(bytes);
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self
            .master
            .lock()
            .expect("pty lock")
            .resize(size(cols, rows));
    }

    pub fn kill(&self) {
        let _ = self.killer.lock().expect("pty lock").kill();
    }

    /// The foreground program, such as `bash` or `htop`. Windows has no foreground group to
    /// ask, so it is the shell's name there.
    #[cfg(windows)]
    pub fn title(&self) -> String {
        self.shell.trim_end_matches(".exe").to_string()
    }

    #[cfg(not(windows))]
    pub fn title(&self) -> String {
        let leader = self.master.lock().expect("pty lock").process_group_leader();
        #[cfg(target_os = "macos")]
        let name = leader.and_then(|pid| crate::macsys::process_name(pid as u32));
        #[cfg(not(target_os = "macos"))]
        let name = leader.and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok());
        name.map(|comm| comm.trim().to_string())
            .filter(|comm| !comm.is_empty())
            .unwrap_or_else(|| self.shell.clone())
    }
}

/// The shell's output, not read yet.
pub struct Reader {
    reader: Box<dyn Read + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Reader {
    /// Reads on a thread: `on_output` gets every chunk, then `Exited` once.
    pub fn start(self, mut on_output: impl FnMut(Output) + Send + 'static) -> Result<()> {
        let Self {
            mut reader,
            mut child,
        } = self;
        std::thread::Builder::new()
            .name("pty-output".into())
            .spawn(move || {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => on_output(Output::Bytes(buf[..n].to_vec())),
                    }
                }
                let code = child
                    .wait()
                    .ok()
                    .and_then(|s| s.signal().is_none().then(|| s.exit_code() as i32));
                on_output(Output::Exited(code));
            })?;
        Ok(())
    }
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols: cols.clamp(10, 500),
        rows: rows.clamp(4, 300),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn shell_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}
