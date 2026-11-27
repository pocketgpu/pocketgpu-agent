//! Gemini CLI (F32): the tools in `~/.gemini/settings.json` (`mcpServers.pocketgpu`), the
//! skill's text as a marked block at the end of `~/.gemini/GEMINI.md`, and alert hooks in
//! the same settings.json. Alerts only: Gemini gets no approval hooks until a live test of
//! its `BeforeTool` hook.
//!
//! The tools are written into settings.json as `gemini mcp add -s user` writes them, not
//! through that command: the hooks go into the same file, and the alerts step must start
//! from the file exactly as the tools step leaves it (as Cursor's steps do).

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

const ME: Assistant = Assistant::Gemini;
/// A turn starts, Gemini waits for the user (a tool permission), a tool ran, a turn ends.
/// The tool hook is the only sign a permission question was answered at the desk, so the
/// phone isn't pinged for it.
pub(super) const EVENTS: [&str; 4] = ["BeforeAgent", "Notification", "AfterTool", "AfterAgent"];
/// Milliseconds Gemini gives the hook; `pgpu hook` itself is done within about 2 s.
const HOOK_TIMEOUT_MS: u64 = 5000;
/// The marked block's first and last lines in GEMINI.md.
pub(super) const START: &str =
    "<!-- PocketGPU: added by pgpu assistants connect, removed by pgpu assistants disconnect -->";
const END: &str = "<!-- end of PocketGPU -->";

pub fn found(ctx: &Ctx) -> bool {
    ctx.runner.find("gemini").is_some() || ctx.places.gemini_dir.is_dir()
}

pub fn plan(ctx: &Ctx) -> Result<Plan> {
    let mut plan = Plan::default();
    let mcp = mcp_step(ctx);
    let after_mcp = match &mcp {
        Ok(Some(Step {
            action: Action::Write { after, .. },
            ..
        })) => Some(after.clone()),
        _ => None,
    };
    match mcp {
        Ok(step) => plan.add(Part::Mcp, step),
        Err(e) => plan
            .notes
            .push(format!("Tools left out for Gemini CLI: {e:#}")),
    }
    plan.add(Part::Skill, block_step(ctx)?);
    // Both change settings.json: the alerts step starts from the file as the tools leave it.
    match hooks_step(ctx, after_mcp) {
        Ok(step) => plan.add(Part::Hooks, step),
        Err(e) => plan.notes.push(format!("Alerts left out: {e:#}")),
    }
    Ok(plan)
}

pub fn status(ctx: &Ctx) -> Result<Status> {
    let settings = files::read(&settings(ctx.places))?.unwrap_or_default();
    let notes = files::read(&notes_file(ctx.places))?.unwrap_or_default();
    Ok(Status {
        assistant: ME,
        found: found(ctx),
        mcp: jsonedit::get(&settings, &["mcpServers", "pocketgpu"]).is_ok_and(|v| v.is_some()),
        skill: notes.contains(START),
        hooks: hooks::missing(&settings, &EVENTS, ME.id()).is_ok_and(|m| m.is_empty()),
        plugin: false,
        ask: false,
        check: false,
        check_blocked: None,
    })
}

pub(super) fn settings(places: &Places) -> PathBuf {
    places.gemini_dir.join("settings.json")
}

pub(super) fn notes_file(places: &Places) -> PathBuf {
    places.gemini_dir.join("GEMINI.md")
}

fn mcp_step(ctx: &Ctx) -> Result<Option<Step>> {
    let path = settings(ctx.places);
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

/// The skill's text without its front matter, between the markers.
pub(super) fn block() -> String {
    let text = skill::TEXT.replace("\r\n", "\n");
    let body = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(text.as_str(), |(_, body)| body);
    format!("{START}\n{}\n{END}\n", body.trim())
}

/// The block at the end of GEMINI.md; `None` when a block of ours is there already (an
/// older one included: it is the user's file, so it isn't rewritten).
fn block_step(ctx: &Ctx) -> Result<Option<Step>> {
    let path = notes_file(ctx.places);
    let before = files::read(&path)?;
    let old = before.clone().unwrap_or_default();
    if old.contains(START) {
        return Ok(None);
    }
    let gap = if old.is_empty() || old.ends_with("\n\n") {
        ""
    } else if old.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let text = format!("{gap}{}", block());
    Ok(Some(Step {
        assistant: ME,
        part: Part::Skill,
        detail: ctx.places.show(&path),
        action: Action::Write {
            path,
            before,
            after: format!("{old}{text}"),
            undo: FileUndo::Appended { text },
        },
    }))
}

/// `base`: the file as the tools step leaves it, else as it is now.
fn hooks_step(ctx: &Ctx, base: Option<String>) -> Result<Option<Step>> {
    let path = settings(ctx.places);
    let before = match base {
        Some(text) => Some(text),
        None => files::read(&path)?,
    };
    let text = before.clone().unwrap_or_default();
    let missing = hooks::missing(&text, &EVENTS, ME.id())
        .with_context(|| format!("{} isn't valid JSON", ctx.places.show(&path)))?;
    if missing.is_empty() {
        return Ok(None);
    }
    let hook = hook(ctx.exe);
    let added = hooks::add(&text, &missing, |_| hooks::group(&hook, None))?;
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

/// Gemini runs hook commands through a shell (PowerShell on Windows) and waits for them.
fn hook(exe: &Path) -> Value {
    json!({
        "name": "pocketgpu",
        "type": "command",
        "command": shell_line(exe, &["hook", ME.id()], cfg!(windows)),
        "timeout": HOOK_TIMEOUT_MS,
    })
}
