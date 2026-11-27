//! Cursor: the tools in `~/.cursor/mcp.json`, the skill in `~/.agents/skills` (Cursor reads
//! it there), and hooks in `~/.cursor/hooks.json`. Cursor has no command line for its MCP
//! settings, so the file is edited in place: every other key stays, in its order.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::Ctx;
use super::files;
use super::hooks::{self, Mode};
use super::jsonedit;
use super::places::{Places, shell_line};
use super::plan::{Action, Assistant, FileUndo, Part, Plan, Status, Step};
use super::skill;

const ME: Assistant = Assistant::Cursor;
/// A prompt goes in, the agent loop ends.
const EVENTS: [&str; 2] = ["beforeSubmitPrompt", "stop"];
/// Before a shell command or an MCP tool runs: the `--check` hooks ("Ask me first", F32).
/// Cursor has no "Ask on my phone": its hooks run before its own prompt shows.
const CHECK_EVENTS: [&str; 2] = ["beforeShellExecution", "beforeMCPExecution"];
/// Seconds Cursor gives the `--check` hook: past the agent's 5 minutes.
const CHECK_TIMEOUT_S: u64 = 330;

pub fn found(ctx: &Ctx) -> bool {
    ctx.places.cursor_dir.is_dir()
        || ctx.runner.find("cursor").is_some()
        || ctx.runner.find("cursor-agent").is_some()
}

pub fn plan(ctx: &Ctx) -> Result<Plan> {
    let mut plan = Plan::default();
    match mcp_step(ctx) {
        Ok(step) => plan.add(Part::Mcp, step),
        Err(e) => plan.notes.push(format!("Tools left out: {e:#}")),
    }
    let skills = ctx.places.agents_dir.join("skills");
    plan.add(
        Part::Skill,
        skill::step(ME, &skills, ctx.places, ctx.manifest)?,
    );
    // Both change hooks.json: "Ask me first" first (it doesn't wait for the alerts
    // question), so the alerts step starts from the file as it leaves it.
    let check = check_step(ctx);
    let after_check = match &check {
        Ok(Some(Step {
            action: Action::Write { after, .. },
            ..
        })) => Some(after.clone()),
        _ => None,
    };
    match hooks_step(ctx, after_check) {
        Ok(step) => plan.add(Part::Hooks, step),
        Err(e) => plan.notes.push(format!("Alerts left out: {e:#}")),
    }
    match check {
        Ok(step) => plan.add(Part::Approvals, step),
        Err(e) => plan.notes.push(format!("Ask me first left out: {e:#}")),
    }
    plan.check_blocked = Some(None);
    Ok(plan)
}

pub fn status(ctx: &Ctx) -> Result<Status> {
    let mcp = files::read(&mcp_file(ctx.places))?.unwrap_or_default();
    let hooks_text = files::read(&hooks_file(ctx.places))?.unwrap_or_default();
    Ok(Status {
        assistant: ME,
        found: found(ctx),
        mcp: jsonedit::get(&mcp, &["mcpServers", "pocketgpu"]).is_ok_and(|v| v.is_some()),
        skill: skill::present(&ctx.places.agents_dir.join("skills")),
        hooks: hooks::missing(&hooks_text, &EVENTS, ME.id()).is_ok_and(|m| m.is_empty()),
        plugin: false,
        ask: false,
        check: hooks::missing_as(&hooks_text, &CHECK_EVENTS, ME.id(), Mode::Check)
            .is_ok_and(|m| m.is_empty()),
        check_blocked: None,
    })
}

fn mcp_file(places: &Places) -> PathBuf {
    places.cursor_dir.join("mcp.json")
}

fn hooks_file(places: &Places) -> PathBuf {
    places.cursor_dir.join("hooks.json")
}

fn mcp_step(ctx: &Ctx) -> Result<Option<Step>> {
    let path = mcp_file(ctx.places);
    let before = files::read(&path)?;
    let text = before.clone().unwrap_or_default();
    let bad = || format!("{} isn't valid JSON", ctx.places.show(&path));
    // Someone's own `pocketgpu` entry stays as they wrote it.
    if jsonedit::get(&text, &["mcpServers", "pocketgpu"])
        .with_context(bad)?
        .is_some()
    {
        return Ok(None);
    }
    let made = if jsonedit::get(&text, &["mcpServers"])?.is_none() {
        vec![vec!["mcpServers".to_string()]]
    } else {
        Vec::new()
    };
    let server = json!({ "command": ctx.exe, "args": ["mcp", "--assistant", ME.id()] });
    let after = jsonedit::insert(&text, &["mcpServers"], "pocketgpu", &server).with_context(bad)?;
    Ok(Some(Step {
        assistant: ME,
        part: Part::Mcp,
        detail: ctx.places.show(&path),
        action: Action::Write {
            path,
            before,
            after,
            undo: FileUndo::Member {
                at: vec!["mcpServers".to_string()],
                key: "pocketgpu".to_string(),
                made,
            },
        },
    }))
}

/// `base`: the file as an earlier step of this run leaves it, else as it is now.
fn hooks_step(ctx: &Ctx, base: Option<String>) -> Result<Option<Step>> {
    let path = hooks_file(ctx.places);
    let before = match base {
        Some(text) => Some(text),
        None => files::read(&path)?,
    };
    let text = with_version(&before);
    let missing = hooks::missing(&text, &EVENTS, ME.id())
        .with_context(|| format!("{} isn't valid JSON", ctx.places.show(&path)))?;
    if missing.is_empty() {
        return Ok(None);
    }
    let hook = hook(ctx.exe);
    let added = hooks::add(&text, &missing, |_| hook.clone())?;
    Ok(Some(Step {
        assistant: ME,
        part: Part::Hooks,
        detail: format!("{}: {}", ctx.places.show(&path), missing.join(", ")),
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

/// A new file starts with the version Cursor's format asks for.
fn with_version(before: &Option<String>) -> String {
    before
        .clone()
        .unwrap_or_else(|| "{\n  \"version\": 1\n}\n".to_string())
}

/// The `--check` hooks, in their own step and undo record; `None` when they are there.
fn check_step(ctx: &Ctx) -> Result<Option<Step>> {
    let path = hooks_file(ctx.places);
    let before = files::read(&path)?;
    let text = with_version(&before);
    let missing = hooks::missing_as(&text, &CHECK_EVENTS, ME.id(), Mode::Check)
        .with_context(|| format!("{} isn't valid JSON", ctx.places.show(&path)))?;
    if missing.is_empty() {
        return Ok(None);
    }
    let hook = check_hook(ctx.exe);
    let added = hooks::add(&text, &missing, |_| hook.clone())?;
    Ok(Some(Step {
        assistant: ME,
        part: Part::Approvals,
        detail: format!("{}: {}", ctx.places.show(&path), missing.join(", ")),
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

/// Cursor runs hooks through PowerShell on Windows and a shell elsewhere.
fn hook(exe: &Path) -> Value {
    json!({ "command": shell_line(exe, &["hook", ME.id()], cfg!(windows)) })
}

/// Cursor waits for it, and stops the call when the hook fails or times out.
fn check_hook(exe: &Path) -> Value {
    json!({
        "command": shell_line(exe, &["hook", ME.id(), "--check"], cfg!(windows)),
        "timeout": CHECK_TIMEOUT_S,
        "failClosed": true,
    })
}
