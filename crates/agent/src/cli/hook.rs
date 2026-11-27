//! `pgpu hook <assistant>`: what an AI assistant's hooks run (F32). Claude Code, Codex,
//! Cursor and Gemini CLI hand their hook's JSON on stdin (Codex's notify program gets it as
//! the last argument instead); this maps it to a turn start, a question for the user, a tool used or a
//! turn end and tells the agent, which decides about the phone (`crate::assistants`).
//!
//! It always exits 0 and never prints, whatever happens: an assistant must never stall, show
//! an error or change course because of PocketGPU. Nothing the assistant or the user wrote
//! (prompt, answer, file names) leaves this process: only the event, the session id and, for
//! a finished tool call, a hash of it.
//!
//! `pgpu hook claude --ask` (F32) is Claude Code's `PermissionRequest` hook: it sends the
//! tool call to the agent, waits while the phone is asked, and prints the phone's Allow or
//! Deny ([`super::hook_answer`]). Anything else, an error or a stopped agent included,
//! prints nothing, so the desk prompt shows as it would without PocketGPU.
//!
//! `pgpu hook <assistant> --check` ("Ask me first") is in [`super::hook_check`]: it never
//! lets a held action through without the phone's Allow.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::process::ExitCode;
use std::time::Duration;

use pocketgpu_proto::local::{AssistantEventKind as Event, Request};
use serde_json::Value;

use super::{hook_answer, hook_check, hook_scan};
use crate::store::paths::Paths;
use pocketgpu_proto::assistants::INPUT_MAX;

/// The most of a JSON argument taken (Codex's notify program), and where week 1's scan of
/// input that isn't whole JSON stops.
pub const MAX_INPUT: usize = 64 * 1024;
/// How long to wait for the hook's input, and then for the agent.
const STDIN_WAIT: Duration = Duration::from_secs(1);
const AGENT_WAIT: Duration = Duration::from_secs(1);
/// Session ids are UUIDs; anything longer is cut.
const SESSION_MAX: usize = 128;

/// The top-level fields read, all short strings. Nothing else is looked at.
pub(super) const FIELDS: [&str; 8] = [
    "hook_event_name",
    "permission_mode",
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

/// Runs with the arguments after `hook`. Always succeeds and prints only an answer, except
/// that a `--check` hook which fails itself exits 2, so the assistant stops the call.
pub fn main(args: &[OsString]) -> ExitCode {
    // A panic would print to stderr; stay quiet even then.
    std::panic::set_hook(Box::new(|_| {}));
    let checking = args.len() > 1 && mode(&args[1..]) == Mode::Check;
    if std::panic::catch_unwind(|| run(args)).is_err() && checking {
        eprintln!("{}", hook_check::FAILED);
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

/// Which hook this is: week 1's alert hook, or one that answers for the phone (F32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Plain,
    /// `--ask`: the assistant would ask at the desk.
    Ask,
    /// `--check`: before every tool call ("Ask me first", T032).
    Check,
}

pub fn mode(rest: &[OsString]) -> Mode {
    if rest.iter().any(|a| a == "--ask") {
        Mode::Ask
    } else if rest.iter().any(|a| a == "--check") {
        Mode::Check
    } else {
        Mode::Plain
    }
}

fn run(args: &[OsString]) {
    let Some(assistant) = args.first().and_then(|a| a.to_str()) else {
        return;
    };
    match mode(&args[1..]) {
        Mode::Plain => {}
        Mode::Ask => return ask(assistant),
        Mode::Check => return hook_check::run(assistant),
    }
    let input = match json_arg(&args[1..]) {
        Some(text) => text,
        None => read_stdin(INPUT_MAX).0,
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

/// `--ask`: asks the agent, which may hold the call for the phone, and prints the answer.
/// Only Claude Code has it for now.
fn ask(assistant: &str) {
    if assistant != "claude" {
        return;
    }
    let (input, oversize) = read_stdin(INPUT_MAX);
    // Too big to check whole: the desk prompt, as without PocketGPU.
    if oversize {
        return;
    }
    let Some(call) = hook_answer::tool_call(&input) else {
        return;
    };
    if call.event != "PermissionRequest" {
        return;
    }
    let Ok(paths) = Paths::discover() else {
        return;
    };
    let req = Request::AssistantAsk {
        assistant: assistant.to_string(),
        session: call.session,
        tool: call.tool,
        input: call.input,
        folder: call.folder,
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let replies = rt.block_on(crate::ipc::client::call_stream(&paths, req));
    let out = hook_answer::final_answer(replies)
        .as_ref()
        .and_then(hook_answer::claude_permission_request);
    if let Some(line) = out {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

/// Codex's notify program gets the JSON as its last argument.
fn json_arg(rest: &[OsString]) -> Option<String> {
    let last = rest.last()?.to_str()?;
    (last.trim_start().starts_with('{') && last.len() <= MAX_INPUT).then(|| last.to_string())
}

/// Up to `limit` bytes of stdin, waiting at most [`STDIN_WAIT`], and whether there was more
/// (or it didn't come in time); empty when stdin is a terminal (someone typed the command),
/// which would wait for ever.
pub(super) fn read_stdin(limit: usize) -> (String, bool) {
    if std::io::stdin().is_terminal() {
        return (String::new(), false);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    // A thread of its own, so a writer that never closes can't hold the hook; it keeps
    // draining after the first part, so the assistant's write doesn't fail on a closed pipe.
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut kept = Vec::new();
        let _ = (&mut stdin).take(limit as u64 + 1).read_to_end(&mut kept);
        let _ = tx.send(kept);
        let _ = std::io::copy(&mut stdin.take(64 * limit as u64), &mut std::io::sink());
    });
    match rx.recv_timeout(STDIN_WAIT) {
        Ok(mut b) => {
            let over = b.len() > limit;
            b.truncate(limit);
            (String::from_utf8_lossy(&b).into_owned(), over)
        }
        Err(_) => (String::new(), true),
    }
}

/// The event in a hook's input, or `None` for anything else (other events, unknown input).
pub fn parse(input: &str) -> Option<Seen> {
    let f = fields(input);
    let get = |k: &str| f.get(k).map(String::as_str);
    let event = match (get("hook_event_name"), get("type")) {
        // Claude Code and Codex hooks, and Cursor's.
        (Some("UserPromptSubmit" | "beforeSubmitPrompt" | "BeforeAgent"), _) => Event::TurnStarted,
        (Some("Notification"), _) => match get("notification_type")? {
            // Gemini CLI's tool permission question.
            "permission_prompt" | "ToolPermission" => Event::NeedsYou,
            "idle_prompt" => Event::Idle,
            _ => return None,
        },
        // A tool ran (or failed): any permission question before it was answered (Claude
        // Code fires nothing on the answer itself). Its hash tells the agent which held
        // "Ask on my phone" request the desk answered.
        (Some("PostToolUse" | "PostToolUseFailure"), _) => match hook_answer::tool_call(input) {
            Some(call) => Event::ToolDone {
                input_hash: call.hash(),
            },
            None => Event::Working,
        },
        // Gemini CLI: a tool ran, so a permission question before it was answered.
        (Some("AfterTool"), _) => Event::Working,
        (Some("Stop" | "AfterAgent"), _) => Event::TurnEnded,
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
pub(super) fn fields(input: &str) -> HashMap<&'static str, String> {
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
