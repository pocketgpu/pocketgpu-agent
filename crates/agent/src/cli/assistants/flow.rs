//! Connect and disconnect: show what changes, ask, change it, say how it went.

use std::collections::HashSet;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::manifest::{self, Manifest, Undone};
use super::plan::{self, Action, Assistant, Change, Part, Step};
use super::{Ctx, DOCS, claude, codex};
use crate::cli::Silent;

#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub dry_run: bool,
    pub yes: bool,
    pub json: bool,
}

/// What happened, for `--json` and for tests.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub found: Vec<Assistant>,
    pub changes: Vec<Outcome>,
    /// Alerts were added in this run.
    pub alerts: bool,
    pub dry_run: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Outcome {
    pub assistant: Assistant,
    pub part: Part,
    pub detail: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const QUESTION: &str = "Ping your phone when an assistant needs you or finishes?";

pub fn connect(
    ctx: &Ctx,
    opts: Opts,
    ask: &mut dyn FnMut(&str) -> bool,
    out: &mut dyn Write,
) -> Result<Report> {
    let plans = super::plans(ctx)?;
    let mut report = Report {
        found: plans.iter().map(|(a, _)| *a).collect(),
        dry_run: opts.dry_run,
        ..Report::default()
    };
    // Each change once: Codex and Cursor share the skill in ~/.agents.
    let mut paths = HashSet::new();
    let mut steps: Vec<Step> = Vec::new();
    for (_, plan) in &plans {
        report.notes.extend(plan.notes.iter().cloned());
        for step in &plan.steps {
            if let Action::Write { path, .. } = &step.action
                && !paths.insert(path.clone())
            {
                continue;
            }
            steps.push(step.clone());
        }
    }
    let (main, alerts): (Vec<Step>, Vec<Step>) =
        steps.into_iter().partition(|s| s.part != Part::Hooks);
    let mut text = String::new();
    if plans.is_empty() {
        text.push_str(&format!(
            "No AI assistants found here (Claude Code, Codex or Cursor).\nFor others, see {DOCS}\n"
        ));
    } else {
        text.push_str(&describe(&report.found, &main, &alerts, &report.notes));
    }
    let nothing = main.is_empty() && alerts.is_empty();
    if plans.is_empty() || nothing || opts.dry_run {
        if !plans.is_empty() && nothing && report.notes.is_empty() {
            text.push_str("Everything is connected already.\n");
        } else if !plans.is_empty() && nothing {
            text.push_str("Nothing else to add.\n");
        } else if opts.dry_run && !plans.is_empty() {
            text.push_str("Nothing was changed (--dry-run).\n");
        }
        return finish(report, &text, opts.json, out);
    }
    if !opts.json {
        write!(out, "{text}")?;
        out.flush()?;
        text.clear();
    }
    report.alerts = !alerts.is_empty() && (opts.yes || ask(QUESTION));
    let mut manifest = Manifest::load(ctx.places)?;
    let with_alerts = report.alerts;
    let chosen = main.iter().chain(alerts.iter().filter(|_| with_alerts));
    for step in chosen {
        let outcome = match plan::apply(step, ctx.places, ctx.runner) {
            Ok(record) => {
                manifest.changes.push(record);
                manifest.save(ctx.places)?;
                None
            }
            Err(e) => Some(format!("{e:#}")),
        };
        report.changes.push(Outcome {
            assistant: step.assistant,
            part: step.part,
            detail: step.detail.clone(),
            ok: outcome.is_none(),
            error: outcome,
        });
    }
    text.push_str(&results(&report));
    if !alerts.is_empty() && !report.alerts {
        text.push_str("No alerts for now. To add them, run pgpu assistants connect again.\n");
    }
    let codex_hooks = report
        .changes
        .iter()
        .any(|c| c.ok && c.assistant == Assistant::Codex && c.part == Part::Hooks);
    if codex_hooks {
        text.push_str("Codex asks once to trust new hooks: type /hooks in Codex.\n");
    }
    finish(report, &text, opts.json, out)
}

pub fn disconnect(
    ctx: &Ctx,
    opts: Opts,
    ask: &mut dyn FnMut(&str) -> bool,
    out: &mut dyn Write,
) -> Result<Report> {
    let mut manifest = Manifest::load(ctx.places)?;
    let mut report = Report::default();
    if manifest.changes.is_empty() {
        let text = "Nothing to take back: pgpu assistants connect hasn't changed anything here.\n";
        return finish(report, text, opts.json, out);
    }
    if !opts.json {
        writeln!(out, "This takes back what pgpu assistants connect added:")?;
        for r in &manifest.changes {
            writeln!(out, "  {:<12} {}", r.assistant.name(), r.detail)?;
        }
        out.flush()?;
    }
    if !opts.yes && !ask("Remove them?") {
        return finish(report, "Nothing was changed.\n", opts.json, out);
    }
    let mut kept = Vec::new();
    for record in manifest.changes.iter().rev() {
        let result = match record.change {
            // Already gone (removed by hand): nothing to run.
            Change::Command { .. } if !mcp_still_there(ctx, record.assistant) => Ok(Undone::Done),
            _ => manifest::undo(record, ctx.places, ctx.runner),
        };
        let error = match result {
            Ok(Undone::Done) => None,
            Ok(Undone::Left(why)) => {
                // A command can be run later; a file someone changed is theirs now.
                if matches!(record.change, Change::Command { .. }) {
                    kept.push(record.clone());
                }
                Some(why)
            }
            Err(e) => {
                kept.push(record.clone());
                Some(format!("{e:#}"))
            }
        };
        report.changes.push(Outcome {
            assistant: record.assistant,
            part: record.part,
            detail: record.detail.clone(),
            ok: error.is_none(),
            error,
        });
    }
    kept.reverse();
    manifest.changes = kept;
    manifest.save(ctx.places)?;
    let mut text = String::new();
    for c in report.changes.iter().filter(|c| !c.ok) {
        let why = c.error.as_deref().unwrap_or("");
        text.push_str(&format!(
            "✗ {:<12} {}: {why}\n",
            c.assistant.name(),
            c.detail
        ));
    }
    text.push_str("Disconnected. Restart your assistants so they let go of PocketGPU.\n");
    finish(report, &text, opts.json, out)
}

fn mcp_still_there(ctx: &Ctx, assistant: Assistant) -> bool {
    match assistant {
        Assistant::Claude => claude::mcp_present(ctx.places),
        Assistant::Codex => codex::mcp_present(ctx.places),
        Assistant::Cursor => true,
    }
}

/// Prints the report (JSON) or the text, and fails quietly when a change failed.
fn finish(report: Report, text: &str, json: bool, out: &mut dyn Write) -> Result<Report> {
    if json {
        writeln!(out, "{}", serde_json::to_string(&report)?)?;
    } else {
        write!(out, "{text}")?;
    }
    out.flush()?;
    if report.changes.iter().any(|c| !c.ok) {
        return Err(Silent(1).into());
    }
    Ok(report)
}

/// The plan in words: each assistant on a line, then what exactly changes under it.
fn describe(found: &[Assistant], main: &[Step], alerts: &[Step], notes: &[String]) -> String {
    let names: Vec<&str> = found.iter().map(|a| a.name()).collect();
    let mut text = format!("Found {}.\n", and_list(&names));
    if !main.is_empty() {
        text.push_str("This adds:\n");
        for a in found {
            let mine: Vec<&Step> = main.iter().filter(|s| s.assistant == *a).collect();
            if mine.is_empty() {
                continue;
            }
            let what = match (
                mine.iter().any(|s| s.part == Part::Mcp),
                mine.iter().any(|s| s.part == Part::Skill),
            ) {
                (true, true) => "PocketGPU's tools and skill",
                (true, false) => "PocketGPU's tools",
                _ => "PocketGPU's skill",
            };
            text.push_str(&format!("  {:<12} {what}\n", a.name()));
            for s in mine {
                text.push_str(&format!("               {}{}\n", s.detail, backup_note(s)));
            }
        }
    }
    if !alerts.is_empty() {
        text.push_str("And, if you say yes, alerts:\n");
        for s in alerts {
            let (name, note) = (s.assistant.name(), backup_note(s));
            text.push_str(&format!("  {name:<12} {}{note}\n", s.detail));
        }
    }
    for note in notes {
        text.push_str(&format!("Note: {note}\n"));
    }
    text
}

/// A file that is there already is copied first, to the agent's data folder.
fn backup_note(step: &Step) -> &'static str {
    match &step.action {
        Action::Write {
            before: Some(_), ..
        } => " (a backup is kept)",
        _ => "",
    }
}

/// One line per assistant: what was added, then anything that failed.
fn results(report: &Report) -> String {
    let mut text = String::new();
    // Only alerts were left, and the answer was no: nothing changed.
    if report.changes.is_empty() {
        return text;
    }
    for a in &report.found {
        let mine: Vec<&Outcome> = report
            .changes
            .iter()
            .filter(|c| c.assistant == *a)
            .collect();
        let done: Vec<&str> = mine
            .iter()
            .filter(|c| c.ok)
            .map(|c| match c.part {
                Part::Mcp => "tools",
                Part::Skill => "skill",
                Part::Hooks => "alerts",
            })
            .collect();
        if !done.is_empty() {
            text.push_str(&format!("✓ {:<12} {}\n", a.name(), and_list(&done)));
        }
        for c in mine.iter().filter(|c| !c.ok) {
            let why = c.error.as_deref().unwrap_or("");
            text.push_str(&format!("✗ {:<12} {}: {why}\n", a.name(), c.detail));
        }
    }
    if report.changes.iter().all(|c| c.ok) {
        text.push_str("Connected. Restart your assistants so they load PocketGPU.\n");
    }
    text.push_str("Undo it with: pgpu assistants disconnect\n");
    text
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
