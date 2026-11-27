//! `pgpu assistants [list | connect | disconnect]` (F32): sets up the AI coding assistants on
//! this computer (Claude Code, Codex, Cursor) with PocketGPU's tools, a skill that says when
//! to use them and, if the user says yes, hooks that ping the phone when an assistant needs
//! them or finishes. Every change is recorded in the agent's data folder, so disconnect takes
//! back exactly those changes and nothing else.
//!
//! Settings name this program by its full path, never a bare `pgpu`: apps started from the
//! Dock don't have `~/.local/bin` on their PATH, and on Windows `pgpu` is a `.cmd` file MCP
//! clients can't start.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::Result;

use crate::cli::AssistantsCmd;
use crate::store::paths::Paths;

mod claude;
mod codex;
mod cursor;
mod files;
mod flow;
mod hooks;
mod jsonedit;
mod jsonparse;
mod manifest;
mod places;
mod plan;
mod skill;
#[cfg(test)]
mod test_env;
#[cfg(test)]
mod tests;

use manifest::Manifest;
use places::{Places, Runner};
use plan::{Assistant, Plan, Status};

/// What the steps need to know about this computer.
struct Ctx<'a> {
    places: &'a Places,
    runner: &'a dyn Runner,
    /// This program, as hooks and MCP settings run it.
    exe: &'a Path,
    /// What connect changed before.
    manifest: &'a Manifest,
}

pub fn run(paths: &Paths, json: bool, cmd: AssistantsCmd) -> Result<()> {
    let places = Places::discover(&paths.data_dir)?;
    let runner = places::System {
        home: places.home.clone(),
    };
    let exe = places::this_program()?;
    let manifest = Manifest::load(&places)?;
    let ctx = Ctx {
        places: &places,
        runner: &runner,
        exe: &exe,
        manifest: &manifest,
    };
    // Questions need someone at a terminal; JSON output is for scripts.
    let can_ask = !json && std::io::stdin().is_terminal();
    let mut ask = |question: &str| can_ask && ask_terminal(question);
    let mut out = std::io::stdout();
    match cmd {
        AssistantsCmd::List => list(&ctx, json, &mut out),
        AssistantsCmd::Connect { dry_run, yes } => {
            let opts = flow::Opts { dry_run, yes, json };
            flow::connect(&ctx, opts, &mut ask, &mut out).map(|_| ())
        }
        AssistantsCmd::Disconnect { yes } => {
            let opts = flow::Opts {
                dry_run: false,
                yes,
                json,
            };
            flow::disconnect(&ctx, opts, &mut ask, &mut out).map(|_| ())
        }
    }
}

/// "[Y/n]": Enter is yes. No input to read is a no.
fn ask_terminal(question: &str) -> bool {
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!();
            false
        }
        Ok(_) => matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes"),
    }
}

/// Each assistant's plan, for those found here.
fn plans(ctx: &Ctx) -> Result<Vec<(Assistant, Plan)>> {
    let mut out = Vec::new();
    for a in Assistant::ALL {
        let plan = match a {
            Assistant::Claude if claude::found(ctx) => claude::plan(ctx)?,
            Assistant::Codex if codex::found(ctx) => codex::plan(ctx)?,
            Assistant::Cursor if cursor::found(ctx) => cursor::plan(ctx)?,
            _ => continue,
        };
        out.push((a, plan));
    }
    Ok(out)
}

fn statuses(ctx: &Ctx) -> Result<Vec<Status>> {
    let mut all = Vec::new();
    for a in Assistant::ALL {
        let mut s = match a {
            Assistant::Claude => claude::status(ctx)?,
            Assistant::Codex => codex::status(ctx)?,
            Assistant::Cursor => cursor::status(ctx)?,
        };
        // The skill in ~/.agents is shared: it says nothing about an assistant not here.
        if !s.found {
            (s.mcp, s.skill, s.hooks, s.plugin) = (false, false, false, false);
        }
        all.push(s);
    }
    Ok(all)
}

fn list(ctx: &Ctx, json: bool, out: &mut dyn Write) -> Result<()> {
    let all = statuses(ctx)?;
    if json {
        writeln!(out, "{}", serde_json::to_string(&all)?)?;
        return Ok(());
    }
    write!(out, "{}", list_words(&all))?;
    Ok(())
}

fn list_words(all: &[Status]) -> String {
    let mark = |on: bool| if on { "✓" } else { "–" };
    let mut text = String::from("AI assistants on this computer:\n");
    for s in all {
        let name = s.assistant.name();
        if !s.found {
            text.push_str(&format!("  {name:<12} not found\n"));
            continue;
        }
        text.push_str(&format!(
            "  {name:<12} tools {}  skill {}  alerts {}{}\n",
            mark(s.mcp),
            mark(s.skill),
            mark(s.hooks),
            if s.plugin { "  (PocketGPU plugin)" } else { "" }
        ));
    }
    let missing = all
        .iter()
        .any(|s| s.found && !(s.mcp && s.skill && s.hooks));
    if missing {
        text.push_str("Connect them: pgpu assistants connect\n");
    } else if all.iter().any(|s| s.found) {
        text.push_str("Turn it off: pgpu assistants disconnect\n");
    } else {
        text.push_str(&format!("For other assistants, see {DOCS}\n"));
    }
    text
}

/// The guide, with the lines for other assistants and for setting up by hand.
const DOCS: &str = "https://pocketgpu.app/docs/ai-assistants";
