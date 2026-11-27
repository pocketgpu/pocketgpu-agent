//! Where each assistant keeps its settings, and how to run the assistants' own command lines.
//! Tests swap both for a temporary home folder and a fake.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
pub struct Places {
    pub home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR` or `~/.claude`: settings.json and skills.
    pub claude_dir: PathBuf,
    /// Where Claude Code keeps user-scope MCP servers.
    pub claude_json: PathBuf,
    /// `$CODEX_HOME` or `~/.codex`.
    pub codex_home: PathBuf,
    pub cursor_dir: PathBuf,
    /// `~/.gemini`: Gemini CLI's settings.json and GEMINI.md.
    pub gemini_dir: PathBuf,
    /// `~/.agents`: skills that several assistants read (Codex, Cursor).
    pub agents_dir: PathBuf,
    /// The agent's data folder: what connect changed, and backups.
    pub data_dir: PathBuf,
}

impl Places {
    pub fn under(home: &Path, data_dir: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            claude_dir: home.join(".claude"),
            claude_json: home.join(".claude.json"),
            codex_home: home.join(".codex"),
            cursor_dir: home.join(".cursor"),
            gemini_dir: home.join(".gemini"),
            agents_dir: home.join(".agents"),
            data_dir: data_dir.to_path_buf(),
        }
    }

    /// This user's places, following the assistants' own environment variables.
    pub fn discover(data_dir: &Path) -> Result<Self> {
        let home = directories::BaseDirs::new()
            .context("could not find a home directory")?
            .home_dir()
            .to_path_buf();
        let mut places = Self::under(&home, data_dir);
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            places.claude_dir = PathBuf::from(&dir);
            places.claude_json = PathBuf::from(dir).join(".claude.json");
        }
        if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()) {
            places.codex_home = PathBuf::from(dir);
        }
        Ok(places)
    }

    pub fn manifest(&self) -> PathBuf {
        crate::assistants::blocked::manifest_path(&self.data_dir)
    }

    pub fn backups(&self) -> PathBuf {
        self.data_dir.join("assistants").join("backups")
    }

    /// `path` with the home folder written as `~`, as people know it.
    pub fn show(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.to_string_lossy().replace('\\', "/")),
            Err(_) => path.display().to_string(),
        }
    }
}

/// What a command printed, and whether it worked.
#[derive(Debug, Clone, Default)]
pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The assistants' own command lines (`claude`, `codex`).
pub trait Runner {
    /// The program's path when it is installed.
    fn find(&self, program: &str) -> Option<PathBuf>;
    fn run(&self, program: &Path, args: &[String]) -> Result<Output>;
}

/// Runs real programs, found on PATH or where their installers put them.
pub struct System {
    pub home: PathBuf,
}

/// A command that hangs (waiting on a login, say) must not hang connect with it.
const RUN_LIMIT: Duration = Duration::from_secs(60);

impl Runner for System {
    fn find(&self, program: &str) -> Option<PathBuf> {
        let exts: &[&str] = if cfg!(windows) {
            &[".exe", ".cmd", ".bat"]
        } else {
            &[""]
        };
        let path_dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default();
        // Where the installers put them, for a shell whose PATH doesn't have it yet.
        let usual = [
            self.home.join(".local/bin"),
            self.home.join(".claude/local"),
            self.home.join(".npm-global/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        path_dirs
            .iter()
            .chain(usual.iter())
            .flat_map(|d| exts.iter().map(move |e| d.join(format!("{program}{e}"))))
            .find(|p| p.is_file())
    }

    fn run(&self, program: &Path, args: &[String]) -> Result<Output> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not start {}", program.display()))?;
        // Read while it runs, so a long answer can't fill the pipe and stall it.
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() > RUN_LIMIT {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{} took too long", program.display());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Ok(Output {
            ok: status.success(),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }
}

fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// This program's own path, links followed, as hooks and MCP settings must name it: apps
/// started from the Dock don't have `~/.local/bin` on their PATH, and on Windows `pgpu` is
/// a `.cmd` file MCP clients can't start.
pub fn this_program() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("could not find this program's path")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(plain_path(exe))
}

/// Windows writes resolved paths as `\\?\C:\…`, which other programs often can't use.
fn plain_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// A command line for a shell: the program's path, quoted when it needs to be, then `args`.
/// Cursor runs hook commands through PowerShell on Windows, which needs `&` before a quoted
/// program; elsewhere `sh` takes single quotes.
pub fn shell_line(program: &Path, args: &[&str], powershell: bool) -> String {
    let path = program.to_string_lossy();
    // A backslash separates folders on Windows, but `sh` reads it as an escape.
    let safe = if cfg!(windows) {
        "/\\:._-+~"
    } else {
        "/:._-+~"
    };
    let plain = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || safe.contains(c));
    let head = if plain {
        path.into_owned()
    } else if powershell {
        format!("& '{}'", path.replace('\'', "''"))
    } else if cfg!(windows) {
        format!("\"{path}\"")
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    };
    std::iter::once(head)
        .chain(args.iter().map(|a| (*a).to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_quoted_only_when_needed() {
        let plain = Path::new("/home/me/.local/bin/pocketgpu");
        assert_eq!(
            shell_line(plain, &["hook", "claude"], false),
            "/home/me/.local/bin/pocketgpu hook claude"
        );
        let spaced = Path::new("/Users/Jo Smith/bin/pocketgpu");
        assert_eq!(
            shell_line(spaced, &["hook", "cursor"], true),
            "& '/Users/Jo Smith/bin/pocketgpu' hook cursor"
        );
        if !cfg!(windows) {
            let quote = Path::new("/Users/O'Neil Pat/pocketgpu");
            assert_eq!(
                shell_line(quote, &["hook", "codex"], false),
                r"'/Users/O'\''Neil Pat/pocketgpu' hook codex"
            );
            let backslash = Path::new(r"/opt/a\b/pocketgpu");
            assert_eq!(
                shell_line(backslash, &["hook", "claude"], false),
                r"'/opt/a\b/pocketgpu' hook claude"
            );
        }
    }

    #[test]
    fn windows_long_paths_are_made_plain() {
        assert_eq!(
            plain_path(PathBuf::from(r"\\?\C:\Users\me\pocketgpu.exe")),
            PathBuf::from(r"C:\Users\me\pocketgpu.exe")
        );
        let unc = PathBuf::from(r"\\?\UNC\server\share\pocketgpu.exe");
        assert_eq!(plain_path(unc.clone()), unc);
    }

    #[test]
    fn home_is_shown_as_tilde() {
        let places = Places::under(Path::new("/home/me"), Path::new("/data"));
        assert_eq!(
            places.show(&places.cursor_dir.join("mcp.json")),
            "~/.cursor/mcp.json"
        );
        assert_eq!(places.show(Path::new("/etc/x")), "/etc/x");
    }
}
