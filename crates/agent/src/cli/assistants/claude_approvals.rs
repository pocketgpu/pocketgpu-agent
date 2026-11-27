//! Claude Code's hooks that let the phone answer (F32): `--ask` in `PermissionRequest`
//! ("Ask on my phone") and `--check` in `PreToolUse` ("Ask me first"), each its own step
//! with its own undo record, so disconnect takes back exactly them.
//!
//! The `--check` entry blocks the call when the hook itself fails (`onFailure: block`),
//! which only Claude Code 2.1.295 and newer understand; an older one would let the call go
//! ahead, so for it no `--check` entry is written and "Ask me first" is blocked
//! ([`crate::assistants::blocked`]). The plugin brings only `--ask`: `--check` blocks the
//! call when it fails, so `connect` always writes it with the agent's full path, never a
//! bare `pgpu` that may not start (an app opened from the Dock, `pgpu.cmd` on Windows).

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::Ctx;
use super::files;
use super::hooks::{self, Mode};
use super::places::shell_line;
use super::plan::{Action, Assistant, FileUndo, Part, Step};

const ME: Assistant = Assistant::Claude;
/// Where Claude Code asks at the desk: the `--ask` hook.
pub const ASK_EVENT: &str = "PermissionRequest";
/// Before every tool call: the `--check` hook.
pub const CHECK_EVENT: &str = "PreToolUse";
/// The tools "Ask me first" looks at: shell commands (Bash, and PowerShell on Windows), file
/// changes and MCP tools.
pub const CHECK_MATCHER: &str = "Bash|PowerShell|Write|Edit|MultiEdit|NotebookEdit|mcp__.*";
/// Seconds Claude Code gives these hooks: past the agent's 5 minutes, so its answer always
/// comes first.
pub const TIMEOUT_S: u64 = 330;

/// Why "Ask me first" can't work with this Claude Code, from `claude --version`.
pub fn version_blocked(ctx: &Ctx, cli: Option<&Path>) -> Option<String> {
    let Some(cli) = cli else {
        return Some(crate::assistants::blocked::claude_not_on_path());
    };
    let out = ctx
        .runner
        .run(cli, &["--version".to_string()])
        .ok()
        .filter(|o| o.ok)
        .map(|o| o.stdout);
    crate::assistants::blocked::claude_blocked(out.as_deref())
}

/// The `--ask` hook in `PermissionRequest`; `None` when it is there already. `base`: the
/// file as an earlier step of this run leaves it.
pub fn ask_step(ctx: &Ctx, base: Option<String>) -> Result<Option<Step>> {
    step(ctx, base, Mode::Ask)
}

/// The `--check` hook in `PreToolUse`; `None` when it is there already.
pub fn check_step(ctx: &Ctx, base: Option<String>) -> Result<Option<Step>> {
    step(ctx, base, Mode::Check)
}

fn step(ctx: &Ctx, base: Option<String>, mode: Mode) -> Result<Option<Step>> {
    let path = super::claude::settings(ctx.places);
    let before = match base {
        Some(text) => Some(text),
        None => files::read(&path)?,
    };
    let text = before.clone().unwrap_or_default();
    let (event, matcher) = match mode {
        Mode::Check => (CHECK_EVENT, Some(CHECK_MATCHER)),
        _ => (ASK_EVENT, None),
    };
    let missing = hooks::missing_as(&text, &[event], ME.id(), mode)
        .with_context(|| format!("{} isn't valid JSON", ctx.places.show(&path)))?;
    if missing.is_empty() {
        return Ok(None);
    }
    let hook = hook(ctx.exe, mode);
    let added = hooks::add(&text, &missing, |_| hooks::group(&hook, matcher))?;
    Ok(Some(Step {
        assistant: ME,
        part: Part::Approvals,
        detail: format!("{}: {event}", ctx.places.show(&path)),
        action: Action::Write {
            path,
            before,
            after: added.text,
            undo: FileUndo::Hooks {
                lists: added.lists,
                hook,
                made: added.made,
            },
        },
    }))
}

/// The file text a step leaves, for the next step on the same file.
pub fn after(step: &Result<Option<Step>>) -> Option<String> {
    match step {
        Ok(Some(Step {
            action: Action::Write { after, .. },
            ..
        })) => Some(after.clone()),
        _ => None,
    }
}

/// Claude Code waits for these (not `async`). `--ask` has no `onFailure`: a failed hook
/// leaves the desk prompt, as wanted. `--check` blocks the call when the hook fails.
pub fn hook(exe: &Path, mode: Mode) -> Value {
    let flag = if mode == Mode::Check {
        "--check"
    } else {
        "--ask"
    };
    let mut hook = if cfg!(windows) {
        json!({ "type": "command", "command": exe, "args": ["hook", ME.id(), flag],
                "timeout": TIMEOUT_S })
    } else {
        let line = shell_line(exe, &["hook", ME.id(), flag], false);
        json!({ "type": "command", "command": line, "timeout": TIMEOUT_S })
    };
    if mode == Mode::Check {
        hook["onFailure"] = json!("block");
    }
    hook
}
