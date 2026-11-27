//! Codex: the tools through `codex mcp add` (or, with no command line on PATH, a table at the
//! end of `config.toml`), the skill in `~/.agents/skills`, and hooks in `~/.codex/hooks.json`.
//! Codex's `notify` program is left alone: it says only when a turn ends, and someone may
//! use it already. Codex asks once to trust new hooks (`/hooks`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::Ctx;
use super::files;
use super::hooks;
use super::places::{Places, shell_line};
use super::plan::{Action, Assistant, FileUndo, Part, Plan, Status, Step};
use super::skill;

const ME: Assistant = Assistant::Codex;
/// A turn starts, a turn ends.
const EVENTS: [&str; 2] = ["UserPromptSubmit", "Stop"];

pub fn found(ctx: &Ctx) -> bool {
    ctx.runner.find("codex").is_some() || ctx.places.codex_home.is_dir()
}

pub fn plan(ctx: &Ctx) -> Result<Plan> {
    let mut plan = Plan::default();
    if mcp_present(ctx.places) {
        plan.present.push(Part::Mcp);
    } else {
        match mcp_step(ctx) {
            Ok(step) => plan.steps.push(step),
            Err(e) => plan.notes.push(format!(
                "Tools left out ({e:#}). Add them with: codex mcp add pocketgpu -- {} mcp",
                ctx.exe.display()
            )),
        }
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
    let text = files::read(&hooks_file(ctx.places))?.unwrap_or_default();
    Ok(Status {
        assistant: ME,
        found: found(ctx),
        mcp: mcp_present(ctx.places),
        skill: skill::present(&ctx.places.agents_dir.join("skills")),
        hooks: hooks::missing(&text, &EVENTS, ME.id()).is_ok_and(|m| m.is_empty()),
        plugin: false,
    })
}

fn config(places: &Places) -> PathBuf {
    places.codex_home.join("config.toml")
}

fn hooks_file(places: &Places) -> PathBuf {
    places.codex_home.join("hooks.json")
}

/// Whether `config.toml` has a `pocketgpu` MCP server.
pub fn mcp_present(places: &Places) -> bool {
    let Ok(Some(text)) = files::read(&config(places)) else {
        return false;
    };
    toml::from_str::<toml::Table>(&text).is_ok_and(|t| {
        t.get("mcp_servers")
            .and_then(|s| s.get("pocketgpu"))
            .is_some()
    })
}

fn mcp_step(ctx: &Ctx) -> Result<Step> {
    let exe = ctx.exe.to_string_lossy().into_owned();
    if let Some(cli) = ctx.runner.find("codex") {
        let args: Vec<String> = ["mcp", "add", "pocketgpu", "--", &exe, "mcp"]
            .map(str::to_string)
            .to_vec();
        return Ok(Step {
            assistant: ME,
            part: Part::Mcp,
            detail: format!("codex {}", args.join(" ")),
            action: Action::Run {
                program: cli,
                args,
                undo: ["codex", "mcp", "remove", "pocketgpu"]
                    .map(str::to_string)
                    .to_vec(),
            },
        });
    }
    // The Codex app and editor extensions keep their command line off PATH: write the
    // table they would. A new table at the very end is valid TOML after anything.
    let path = config(ctx.places);
    let before = files::read(&path)?;
    let old = before.clone().unwrap_or_default();
    let gap = if old.is_empty() || old.ends_with("\n\n") {
        ""
    } else if old.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let table = format!(
        "{gap}[mcp_servers.pocketgpu]\ncommand = {}\nargs = [\"mcp\"]\n",
        toml::Value::String(exe)
    );
    let after = format!("{old}{table}");
    toml::from_str::<toml::Table>(&after)
        .with_context(|| format!("{} can't take the table", ctx.places.show(&path)))?;
    Ok(Step {
        assistant: ME,
        part: Part::Mcp,
        detail: ctx.places.show(&path),
        action: Action::Write {
            path,
            before,
            after,
            undo: FileUndo::Appended { text: table },
        },
    })
}

fn hooks_step(ctx: &Ctx) -> Result<Option<Step>> {
    let path = hooks_file(ctx.places);
    let before = files::read(&path)?;
    let text = before.clone().unwrap_or_default();
    let missing = hooks::missing(&text, &EVENTS, ME.id())
        .with_context(|| format!("{} isn't valid JSON", ctx.places.show(&path)))?;
    if missing.is_empty() {
        return Ok(None);
    }
    let hook = hook(ctx);
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

/// Runs in the background (`async`): Codex never waits on it.
fn hook(ctx: &Ctx) -> Value {
    let line = shell_line(ctx.exe, &["hook", ME.id()], false);
    json!({ "type": "command", "command": line, "async": true })
}
