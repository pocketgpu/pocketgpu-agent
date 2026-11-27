//! What connect changes, step by step, and how each step is recorded so disconnect can take
//! back exactly that.

use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::files;
use super::places::{Places, Runner};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Assistant {
    Claude,
    Codex,
    Cursor,
    /// Alerts only (F32): no approval hooks yet.
    Gemini,
}

impl Assistant {
    pub const ALL: [Assistant; 4] = [
        Assistant::Claude,
        Assistant::Codex,
        Assistant::Cursor,
        Assistant::Gemini,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Assistant::Claude => "Claude Code",
            Assistant::Codex => "Codex",
            Assistant::Cursor => "Cursor",
            Assistant::Gemini => "Gemini CLI",
        }
    }

    /// The name `pgpu hook` takes.
    pub fn id(self) -> &'static str {
        match self {
            Assistant::Claude => "claude",
            Assistant::Codex => "codex",
            Assistant::Cursor => "cursor",
            Assistant::Gemini => "gemini",
        }
    }
}

/// The things connect sets up for an assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    /// PocketGPU's MCP server: the tools.
    Mcp,
    /// The skill that says when to use them.
    Skill,
    /// The hooks that ping the phone; added only after the user says yes.
    Hooks,
    /// The hooks that let the phone answer (F32): `--ask`. They do nothing until "Ask on my
    /// phone" is on, and have their own undo record.
    Approvals,
}

/// What connect would do for one assistant.
#[derive(Debug, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Parts already set up.
    pub present: Vec<Part>,
    /// For the user: a step to take by hand, or why something is skipped.
    pub notes: Vec<String>,
    /// Checked for this assistant (F32): why "Ask me first" can't work for it, or `None`
    /// when it can. Recorded in the manifest for the agent.
    pub check_blocked: Option<Option<String>>,
}

impl Plan {
    pub fn add(&mut self, part: Part, step: Option<Step>) {
        match step {
            Some(s) => self.steps.push(s),
            None => self.present.push(part),
        }
    }
}

/// What `pgpu assistants` lists for one assistant.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub assistant: Assistant,
    pub found: bool,
    pub mcp: bool,
    pub skill: bool,
    pub hooks: bool,
    /// Claude Code: the PocketGPU plugin brings all three.
    pub plugin: bool,
    /// Its `--ask` hook is in place: the phone can answer what it would ask at the desk.
    pub ask: bool,
    /// Its `--check` hook is in place ("Ask me first").
    pub check: bool,
    /// Why "Ask me first" can't work for it here, such as Claude Code being too old.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_blocked: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub assistant: Assistant,
    pub part: Part,
    /// The command or file, for people.
    pub detail: String,
    pub action: Action,
}

#[derive(Debug, Clone)]
pub enum Action {
    /// Run an assistant's own command line; `undo` (program name first) takes it back.
    Run {
        program: PathBuf,
        args: Vec<String>,
        undo: Vec<String>,
    },
    /// Write a file that held `before` (`None`: there was none).
    Write {
        path: PathBuf,
        before: Option<String>,
        after: String,
        undo: FileUndo,
    },
}

/// How to take a file change back when the file changed since (the assistant saved its own
/// settings, say), so its old copy can't simply go back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum FileUndo {
    /// The whole file is ours (the skill): it goes, unless someone edited it.
    Delete,
    /// A key we added to the object at `at`. `made` are objects connect created on the way.
    Member {
        at: Vec<String>,
        key: String,
        #[serde(default)]
        made: Vec<Vec<String>>,
    },
    /// Our hook in each list: inside a group (Claude Code, Codex) or on its own (Cursor).
    Hooks {
        lists: Vec<Vec<String>>,
        hook: Value,
        #[serde(default)]
        made: Vec<Vec<String>>,
    },
    /// Text added at the end of the file.
    Appended { text: String },
}

/// One change connect made, as kept in the agent's data folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub assistant: Assistant,
    pub part: Part,
    pub detail: String,
    #[serde(flatten)]
    pub change: Change,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    Command {
        undo: Vec<String>,
    },
    File {
        path: PathBuf,
        /// The file as it was, or `None` when connect made it.
        backup: Option<PathBuf>,
        /// The hash of what connect wrote: unchanged since means the backup can go back as is.
        written: String,
        /// Folders connect made for it.
        #[serde(default)]
        made_dirs: Vec<PathBuf>,
        undo: FileUndo,
    },
}

/// Makes the change and says how it was made.
pub fn apply(step: &Step, places: &Places, runner: &dyn Runner) -> Result<Record> {
    let change = match &step.action {
        Action::Run {
            program,
            args,
            undo,
        } => {
            let out = runner.run(program, args)?;
            if !out.ok {
                let why = if out.stderr.trim().is_empty() {
                    &out.stdout
                } else {
                    &out.stderr
                };
                bail!("{}", why.trim().lines().next().unwrap_or("it failed"));
            }
            Change::Command { undo: undo.clone() }
        }
        Action::Write {
            path,
            before,
            after,
            undo,
        } => {
            // Settled while the question was open; someone may have changed it since.
            if files::read(path)? != *before {
                bail!("{} changed meanwhile; run this again", places.show(path));
            }
            let made_dirs = match path.parent() {
                Some(dir) => files::make_dirs(dir)?,
                None => Vec::new(),
            };
            let backup = match before {
                Some(text) => Some(backup(places, path, text)?),
                None => None,
            };
            files::write(path, after)?;
            Change::File {
                path: path.clone(),
                backup,
                written: files::hash(after),
                made_dirs,
                undo: undo.clone(),
            }
        }
    };
    Ok(Record {
        assistant: step.assistant,
        part: step.part,
        detail: step.detail.clone(),
        change,
    })
}

/// Keeps a copy of a file before changing it, in the agent's data folder.
fn backup(places: &Places, path: &std::path::Path, text: &str) -> Result<PathBuf> {
    let dir = places.backups();
    files::make_dirs(&dir)?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = |p: Option<&std::path::Path>| {
        p.and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().trim_start_matches('.').to_string())
            .unwrap_or_default()
    };
    let base = format!("{ms}-{}-{}", name(path.parent()), name(Some(path)));
    let mut at = dir.join(&base);
    let mut n = 1;
    while at.exists() {
        n += 1;
        at = dir.join(format!("{base}.{n}"));
    }
    // Settings can hold keys (an MCP server's env), so the copy is private.
    crate::store::paths::write_private(&at, text.as_bytes())?;
    Ok(at)
}
