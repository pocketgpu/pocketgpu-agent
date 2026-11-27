//! Cursor: the tools in `~/.cursor/mcp.json`, the skill in `~/.agents/skills` (Cursor reads
//! it there), and hooks in `~/.cursor/hooks.json`. Cursor has no command line for its MCP
//! settings, so the file is edited in place: every other key stays, in its order.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::Ctx;
use super::files;
use super::hooks;
use super::jsonedit;
use super::places::{Places, shell_line};
use super::plan::{Action, Assistant, FileUndo, Part, Plan, Status, Step};
use super::skill;

const ME: Assistant = Assistant::Cursor;
/// A prompt goes in, the agent loop ends.
const EVENTS: [&str; 2] = ["beforeSubmitPrompt", "stop"];

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
    match hooks_step(ctx) {
        Ok(step) => plan.add(Part::Hooks, step),
        Err(e) => plan.notes.push(format!("Alerts left out: {e:#}")),
    }
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
    let server = json!({ "command": ctx.exe, "args": ["mcp"] });
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

fn hooks_step(ctx: &Ctx) -> Result<Option<Step>> {
    let path = hooks_file(ctx.places);
    let before = files::read(&path)?;
    // A new file starts with the version Cursor's format asks for.
    let text = before
        .clone()
        .unwrap_or_else(|| "{\n  \"version\": 1\n}\n".to_string());
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

/// Cursor runs hooks through PowerShell on Windows and a shell elsewhere.
fn hook(exe: &Path) -> Value {
    json!({ "command": shell_line(exe, &["hook", ME.id()], cfg!(windows)) })
}
