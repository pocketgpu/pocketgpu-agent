//! `pgpu hook <assistant> --check` (F32, "Ask me first"): runs before every tool call
//! (Claude Code's `PreToolUse`, Cursor's `beforeShellExecution` and `beforeMCPExecution`).
//! It sends the call to the agent, which passes it at once unless it matches a chosen kind
//! or the owner's own words, and then holds it for the phone.
//!
//! Nothing here may let a held action through without the phone's Allow: a held action that
//! loses its agent, input too big to check, or an agent that can't be reached while the
//! guard file says armed all get the fail-closed answer (research.md §3 and §4). Only the
//! agent's `Pass`, or an unreachable agent with no guard file, prints nothing.

use anyhow::Result;
use pocketgpu_proto::assistants::{Answer, Ended, INPUT_MAX};
use pocketgpu_proto::local::{Request, Response};
use serde_json::{Value, json};

use super::hook_answer;
use crate::assistants::answers::fail_closed;
use crate::ipc::client::HeldLost;
use crate::store::paths::Paths;

/// What one `--check` call asks the agent, and the mode its fail-closed answer depends on.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub permission_mode: String,
    pub req: Request,
}

/// What a hook says on stderr when it fails itself; the assistant then stops the call
/// (`onFailure: block`, `failClosed`, exit code 2).
pub const FAILED: &str = "PocketGPU couldn't check this action, so it was stopped. \
     To take PocketGPU's hooks out, run: pgpu assistants disconnect. \
     Without pgpu, remove them by hand: the hooks whose command ends in \
     `hook claude --check` or `hook cursor` in ~/.claude/settings.json and \
     ~/.cursor/hooks.json, or the plugin (claude plugin uninstall pocketgpu)";

/// Reads the hook's input, asks the agent and prints the answer.
pub fn run(assistant: &str) {
    if !matches!(assistant, "claude" | "cursor") {
        return;
    }
    let (text, oversize) = super::hook::read_stdin(INPUT_MAX);
    let Some(check) = request(assistant, &text, oversize) else {
        return;
    };
    let mode = check.permission_mode.clone();
    let answer = match (Paths::discover(), runtime()) {
        (Ok(paths), Some(rt)) => {
            let replies = rt.block_on(crate::ipc::client::call_stream(&paths, check.req));
            decide(assistant, &mode, replies, || {
                crate::assistants::guard::armed(&paths)
            })
        }
        // No data folder to find the guard in, or no way to ask: the safe side.
        _ => Some(fail_closed(assistant, &mode, Ended::NoPhone)),
    };
    if let Some(line) = answer.and_then(|a| output(assistant, &mode, &a)) {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

fn runtime() -> Option<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()
}

/// The request for this hook's input; `None` when it isn't a call `--check` answers (another
/// event). Input that is too big or doesn't parse goes as `oversize`, which the agent never
/// passes while "Ask me first" is on.
pub fn request(assistant: &str, text: &str, oversize: bool) -> Option<Check> {
    match assistant {
        "claude" => claude(text, oversize),
        "cursor" => cursor(text, oversize),
        _ => None,
    }
}

fn claude(text: &str, oversize: bool) -> Option<Check> {
    let call = if oversize {
        None
    } else {
        hook_answer::tool_call(text)
    };
    if let Some(call) = call {
        if call.event != "PreToolUse" {
            return None;
        }
        return Some(check(
            "claude",
            &call.permission_mode,
            call.session,
            call.tool,
            call.input,
            call.folder,
        ));
    }
    let f = super::hook::fields(text);
    // Whole JSON for another event: not ours to answer.
    if !oversize && f.get("hook_event_name").is_some_and(|e| e != "PreToolUse") {
        return None;
    }
    let mode = f.get("permission_mode").cloned().unwrap_or_default();
    let session = f.get("session_id").cloned().unwrap_or_default();
    Some(unknown("claude", &mode, session))
}

fn cursor(text: &str, oversize: bool) -> Option<Check> {
    let parsed = if oversize {
        None
    } else {
        match serde_json::from_str::<Value>(text.trim()) {
            Ok(Value::Object(map)) => Some(map),
            _ => None,
        }
    };
    let Some(map) = parsed else {
        let f = super::hook::fields(text);
        let session = f.get("conversation_id").cloned().unwrap_or_default();
        return Some(unknown("cursor", "", session));
    };
    let text_of = |k: &str| map.get(k).and_then(Value::as_str).map(str::to_string);
    let session = text_of("conversation_id").unwrap_or_default();
    let folder = text_of("cwd")
        .or_else(|| {
            map.get("workspace_roots")
                .and_then(|r| r.get(0))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let (tool, input) = match text_of("hook_event_name").as_deref() {
        Some("beforeShellExecution") => match text_of("command") {
            Some(cmd) => ("shell".to_string(), json!({ "command": cmd }).to_string()),
            None => return Some(unknown("cursor", "", session)),
        },
        Some("beforeMCPExecution") => {
            let tool = text_of("tool_name").unwrap_or_else(|| "MCP".into());
            // Cursor sends the tool's input as JSON text; either way the agent sees it whole.
            let input = match map.get("tool_input") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            (tool, input)
        }
        _ => return None,
    };
    Some(check("cursor", "", session, tool, input, folder))
}

fn check(
    assistant: &str,
    mode: &str,
    session: String,
    tool: String,
    input: String,
    folder: String,
) -> Check {
    Check {
        permission_mode: mode.to_string(),
        req: Request::AssistantCheck {
            assistant: assistant.to_string(),
            session,
            permission_mode: mode.to_string(),
            tool,
            input,
            oversize: false,
            folder,
        },
    }
}

/// A call that couldn't be read whole: the agent passes it only while nothing is armed.
fn unknown(assistant: &str, mode: &str, session: String) -> Check {
    let mut c = check(
        assistant,
        mode,
        session,
        String::new(),
        String::new(),
        String::new(),
    );
    if let Request::AssistantCheck { oversize, .. } = &mut c.req {
        *oversize = true;
    }
    c
}

/// The answer to print from the agent's replies. A held action that lost its answer gets
/// the fail-closed answer; so does an agent that can't be reached (down, too old, confused)
/// while the guard file says armed. Otherwise an unreachable agent changes nothing.
pub fn decide(
    assistant: &str,
    permission_mode: &str,
    replies: Result<(Response, Option<Response>)>,
    armed: impl FnOnce() -> bool,
) -> Option<Answer> {
    let fail = || fail_closed(assistant, permission_mode, Ended::NoPhone);
    match replies {
        Ok((Response::AssistantAnswer(a), None)) => Some(a),
        Ok((Response::AssistantHeld { .. }, Some(Response::AssistantAnswer(a)))) => Some(a),
        Ok((Response::AssistantHeld { .. }, _)) => Some(fail()),
        Err(e) if e.downcast_ref::<HeldLost>().is_some() => Some(fail()),
        _ => armed().then(fail),
    }
}

/// The answer as this assistant's hook JSON.
pub fn output(assistant: &str, permission_mode: &str, answer: &Answer) -> Option<String> {
    match assistant {
        "claude" => hook_answer::claude_pre_tool_use(answer, permission_mode),
        "cursor" => hook_answer::cursor_permission(answer),
        _ => None,
    }
}

#[cfg(test)]
#[path = "hook_check_tests.rs"]
mod tests;
