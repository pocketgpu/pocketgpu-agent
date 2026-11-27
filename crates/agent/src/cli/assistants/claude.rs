//! Claude Code: the tools through its own command line (user scope, so every project has
//! them), the skill in `~/.claude/skills`, and hooks in `~/.claude/settings.json`. The
//! PocketGPU plugin brings all three, so with it installed nothing is added here.

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

const ME: Assistant = Assistant::Claude;
/// A turn starts, Claude Code waits for the user, a turn ends, and a tool ran: the only sign
/// that a permission question was answered at the desk, so the phone isn't pinged for it.
const EVENTS: [&str; 4] = ["UserPromptSubmit", "Notification", "Stop", "PostToolUse"];
/// Only the waits that need the user: a permission question, or idle at the prompt.
const NOTIFICATIONS: &str = "permission_prompt|idle_prompt";

pub fn found(ctx: &Ctx) -> bool {
    ctx.runner.find("claude").is_some() || ctx.places.claude_dir.is_dir()
}

pub fn plan(ctx: &Ctx) -> Result<Plan> {
    let mut plan = Plan::default();
    let cli = ctx.runner.find("claude");
    if plugin(ctx, cli.as_deref()) {
        plan.present = vec![Part::Mcp, Part::Skill, Part::Hooks];
        return Ok(plan);
    }
    let exe = ctx.exe.to_string_lossy().into_owned();
    if mcp_present(ctx.places) {
        plan.present.push(Part::Mcp);
    } else if let Some(cli) = cli {
        let args: Vec<String> = [
            "mcp",
            "add",
            "--scope",
            "user",
            "pocketgpu",
            "--",
            &exe,
            "mcp",
        ]
        .map(str::to_string)
        .to_vec();
        plan.steps.push(Step {
            assistant: ME,
            part: Part::Mcp,
            detail: format!("claude {}", args.join(" ")),
            action: Action::Run {
                program: cli,
                args,
                undo: ["claude", "mcp", "remove", "--scope", "user", "pocketgpu"]
                    .map(str::to_string)
                    .to_vec(),
            },
        });
    } else {
        plan.notes.push(format!(
            "Claude Code's command line isn't on PATH. Add the tools with: \
             claude mcp add --scope user pocketgpu -- {exe} mcp"
        ));
    }
    let skills = ctx.places.claude_dir.join("skills");
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
    let plugin = plugin(ctx, ctx.runner.find("claude").as_deref());
    let settings = files::read(&settings(ctx.places))?.unwrap_or_default();
    Ok(Status {
        assistant: ME,
        found: found(ctx),
        mcp: plugin || mcp_present(ctx.places),
        skill: plugin || skill::present(&ctx.places.claude_dir.join("skills")),
        hooks: plugin || hooks::missing(&settings, &EVENTS, ME.id()).is_ok_and(|m| m.is_empty()),
        plugin,
    })
}

fn settings(places: &Places) -> PathBuf {
    places.claude_dir.join("settings.json")
}

/// Whether the tools are set up for every project (user scope, in `~/.claude.json`).
pub fn mcp_present(places: &Places) -> bool {
    let Ok(Some(text)) = files::read(&places.claude_json) else {
        return false;
    };
    serde_json::from_str::<Value>(&text).is_ok_and(|v| v["mcpServers"]["pocketgpu"].is_object())
}

/// Whether the PocketGPU plugin is installed and on: from Claude Code's own list, or, when
/// that can't be had, from what its settings turn on.
fn plugin(ctx: &Ctx, cli: Option<&Path>) -> bool {
    let args = ["plugin", "list", "--json"].map(str::to_string);
    if let Some(cli) = cli
        && let Ok(out) = ctx.runner.run(cli, &args)
        && out.ok
        && let Ok(Value::Array(list)) = serde_json::from_str::<Value>(&out.stdout)
    {
        return list.iter().any(|p| {
            p["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("pocketgpu@"))
                && p["enabled"] != json!(false)
        });
    }
    let Ok(Some(text)) = files::read(&settings(ctx.places)) else {
        return false;
    };
    jsonedit::read(&text).is_ok_and(|v| {
        v["enabledPlugins"].as_object().is_some_and(|m| {
            m.iter()
                .any(|(k, on)| k.starts_with("pocketgpu@") && on == true)
        })
    })
}

fn hooks_step(ctx: &Ctx) -> Result<Option<Step>> {
    let path = settings(ctx.places);
    let before = files::read(&path)?;
    let text = before.clone().unwrap_or_default();
    let missing = hooks::missing(&text, &EVENTS, ME.id())
        .with_context(|| format!("{} isn't valid JSON", ctx.places.show(&path)))?;
    if missing.is_empty() {
        return Ok(None);
    }
    let hook = hook(ctx.exe);
    let added = hooks::add(&text, &missing, |event| {
        let matcher = (event == "Notification").then_some(NOTIFICATIONS);
        hooks::group(&hook, matcher)
    })?;
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

/// Runs in the background (`async`): Claude Code never waits on it.
fn hook(exe: &Path) -> Value {
    if cfg!(windows) {
        // No shell in between: Git Bash and PowerShell quote paths differently.
        json!({ "type": "command", "command": exe, "args": ["hook", ME.id()], "async": true })
    } else {
        let line = shell_line(exe, &["hook", ME.id()], false);
        json!({ "type": "command", "command": line, "async": true })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plugin brings its own hooks (under `oss/public` here, at the root of the public
    /// repository); they must listen for the same events as the ones `connect` writes.
    #[test]
    fn the_plugin_hooks_the_same_events() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rel = "plugins/pocketgpu/hooks/hooks.json";
        let copies: Vec<Value> = [
            crate_dir.join("../../oss/public").join(rel),
            crate_dir.join("../..").join(rel),
        ]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|text| serde_json::from_str(&text).unwrap())
        .collect();
        assert!(!copies.is_empty(), "the plugin's hooks.json is missing");
        for copy in copies {
            let hooks = copy["hooks"].as_object().unwrap();
            let mut events: Vec<&str> = hooks.keys().map(String::as_str).collect();
            let mut want = EVENTS.to_vec();
            events.sort_unstable();
            want.sort_unstable();
            assert_eq!(events, want);
            assert_eq!(hooks["Notification"][0]["matcher"], NOTIFICATIONS);
            for (event, groups) in hooks {
                let hook = &groups[0]["hooks"][0];
                assert_eq!(hook["command"], "pgpu hook claude", "{event}");
                assert_eq!(hook["async"], true, "{event}");
            }
        }
    }
}
