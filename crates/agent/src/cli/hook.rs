//! `pgpu hook <assistant>`: what an AI assistant's hooks run (F32). Claude Code, Codex and
//! Cursor hand their hook's JSON on stdin (Codex's notify program gets it as the last
//! argument instead); this maps it to a turn start, a question for the user, a tool used or a
//! turn end and tells the agent, which decides about the phone (`crate::assistants`).
//!
//! It always exits 0 and never prints, whatever happens: an assistant must never stall, show
//! an error or change course because of PocketGPU. Nothing the assistant or the user wrote
//! (prompt, answer, file names) leaves this process: only the event and the session id.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::process::ExitCode;
use std::time::Duration;

use pocketgpu_proto::local::{AssistantEventKind as Event, Request};
use serde_json::Value;

use super::hook_scan;
use crate::store::paths::Paths;

/// The most input read; the rest is drained and dropped.
pub const MAX_INPUT: usize = 64 * 1024;
/// How long to wait for the hook's input, and then for the agent.
const STDIN_WAIT: Duration = Duration::from_secs(1);
const AGENT_WAIT: Duration = Duration::from_secs(1);
/// Session ids are UUIDs; anything longer is cut.
const SESSION_MAX: usize = 128;

/// The top-level fields read, all short strings. Nothing else is looked at.
pub(super) const FIELDS: [&str; 7] = [
    "hook_event_name",
    "notification_type",
    "session_id",
    "conversation_id",
    "thread-id",
    "type",
    "status",
];

/// What one hook call tells the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub session: String,
    pub event: Event,
}

/// Runs with the arguments after `hook`. Always succeeds, prints nothing.
pub fn main(args: &[OsString]) -> ExitCode {
    // A panic would print to stderr; stay quiet even then.
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(|| run(args));
    ExitCode::SUCCESS
}

fn run(args: &[OsString]) {
    let Some(assistant) = args.first().and_then(|a| a.to_str()) else {
        return;
    };
    let input = match json_arg(&args[1..]) {
        Some(text) => text,
        None => read_stdin(),
    };
    let Some(seen) = parse(&input) else {
        return;
    };
    let Ok(paths) = Paths::discover() else {
        return;
    };
    let req = Request::AssistantEvent {
        assistant: assistant.to_string(),
        session: seen.session,
        event: seen.event,
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    // The agent answers at once; a stopped or busy one is simply not told.
    rt.block_on(async {
        let _ = tokio::time::timeout(AGENT_WAIT, crate::ipc::client::call(&paths, req)).await;
    });
}

/// Codex's notify program gets the JSON as its last argument.
fn json_arg(rest: &[OsString]) -> Option<String> {
    let last = rest.last()?.to_str()?;
    (last.trim_start().starts_with('{') && last.len() <= MAX_INPUT).then(|| last.to_string())
}

/// Up to [`MAX_INPUT`] of stdin, waiting at most [`STDIN_WAIT`]; empty when stdin is a
/// terminal (someone typed the command), which would wait for ever.
fn read_stdin() -> String {
    if std::io::stdin().is_terminal() {
        return String::new();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    // A thread of its own, so a writer that never closes can't hold the hook; it keeps
    // draining after the first part, so the assistant's write doesn't fail on a closed pipe.
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut kept = Vec::new();
        let _ = (&mut stdin).take(MAX_INPUT as u64).read_to_end(&mut kept);
        let _ = tx.send(kept);
        let _ = std::io::copy(&mut stdin.take(64 * MAX_INPUT as u64), &mut std::io::sink());
    });
    rx.recv_timeout(STDIN_WAIT)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// The event in a hook's input, or `None` for anything else (other events, unknown input).
pub fn parse(input: &str) -> Option<Seen> {
    let f = fields(input);
    let get = |k: &str| f.get(k).map(String::as_str);
    let event = match (get("hook_event_name"), get("type")) {
        // Claude Code and Codex hooks, and Cursor's.
        (Some("UserPromptSubmit" | "beforeSubmitPrompt"), _) => Event::TurnStarted,
        (Some("Notification"), _) => match get("notification_type")? {
            "permission_prompt" => Event::NeedsYou,
            "idle_prompt" => Event::Idle,
            _ => return None,
        },
        // A tool ran: any permission question before it was answered (Claude Code fires
        // nothing on the answer itself).
        (Some("PostToolUse"), _) => Event::Working,
        (Some("Stop"), _) => Event::TurnEnded,
        // Cursor: stopped by the user at the desk means they already know.
        (Some("stop"), _) if get("status") == Some("aborted") => return None,
        (Some("stop"), _) => Event::TurnEnded,
        // Codex's notify program.
        (None, Some("agent-turn-complete")) => Event::TurnEnded,
        _ => return None,
    };
    let session = get("session_id")
        .or(get("conversation_id"))
        .or(get("thread-id"))
        .unwrap_or("");
    Some(Seen {
        session: session.chars().take(SESSION_MAX).collect(),
        event,
    })
}

/// The [`FIELDS`] that are top-level strings. Input cut at [`MAX_INPUT`] (a long pasted
/// prompt, a long last answer) isn't valid JSON, so the part before the cut is scanned.
fn fields(input: &str) -> HashMap<&'static str, String> {
    match serde_json::from_str::<Value>(input.trim()) {
        Ok(Value::Object(map)) => FIELDS
            .iter()
            .filter_map(|k| Some((*k, map.get(*k)?.as_str()?.to_string())))
            .collect(),
        Ok(_) => HashMap::new(),
        Err(_) => hook_scan::top_level_strings(input),
    }
}

#[cfg(test)]
#[path = "hook_tests.rs"]
mod tests;
