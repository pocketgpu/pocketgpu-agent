//! `pgpu hook --ask` and `--check` (F32): the tool call in a hook's input, and the agent's
//! answer as the assistant's JSON (research.md §3). For `--ask`, `Pass`, `NoDecision` and
//! every error print nothing, so the assistant asks at the desk as it would without
//! PocketGPU. For `--check`, only `Pass` prints nothing; every other way a held action ends
//! is a deny or the desk prompt. Only a phone's Allow ever prints an allow.

use anyhow::Result;
use pocketgpu_proto::assistants::{Answer, Ended};
use pocketgpu_proto::local::Response;
use serde_json::{Value, json};

use crate::assistants::answers::{COULDNT_ASK, Hash, NO_ANSWER, desk_prompt_shows, input_hash};
use crate::assistants::hold::TOOL_MAX;

/// Session ids are UUIDs; anything longer is cut (as `hook::parse` does).
const SESSION_MAX: usize = 128;

/// A tool call from a Claude Code hook's input (`PermissionRequest`, `PostToolUse`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub event: String,
    pub session: String,
    pub permission_mode: String,
    pub tool: String,
    /// `tool_input` as compact JSON: what the agent classifies and hashes.
    pub input: String,
    pub folder: String,
}

impl ToolCall {
    /// The hash the agent gives this call when it holds it, so a later `PostToolUse` for
    /// the same call can say the desk answered it.
    pub fn hash(&self) -> Hash {
        let tool: String = self.tool.chars().take(TOOL_MAX).collect();
        input_hash(&tool, &self.input)
    }
}

/// The call in a hook's whole input; `None` when it isn't JSON or names no tool.
pub fn tool_call(input: &str) -> Option<ToolCall> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(input.trim()) else {
        return None;
    };
    let text = |k: &str| map.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let tool = map.get("tool_name")?.as_str()?.to_string();
    let input = serde_json::to_string(map.get("tool_input").unwrap_or(&Value::Null)).ok()?;
    Some(ToolCall {
        event: text("hook_event_name"),
        session: text("session_id").chars().take(SESSION_MAX).collect(),
        permission_mode: text("permission_mode"),
        tool,
        input,
        folder: text("cwd"),
    })
}

/// The answer in the agent's replies: the first one, or the final one for a held action.
/// Anything else (an error, an older agent, a stream that closed) is `None`: no answer.
pub fn final_answer(replies: Result<(Response, Option<Response>)>) -> Option<Answer> {
    match replies.ok()? {
        (Response::AssistantAnswer(a), None) => Some(a),
        (Response::AssistantHeld { .. }, Some(Response::AssistantAnswer(a))) => Some(a),
        _ => None,
    }
}

/// What Claude Code's `PermissionRequest` hook prints: an allow or a deny decided on the
/// phone, else nothing (the desk prompt stays).
pub fn claude_permission_request(answer: &Answer) -> Option<String> {
    let decision = match answer {
        Answer::Allow => json!({ "behavior": "allow" }),
        Answer::Deny { message } => json!({ "behavior": "deny", "message": message }),
        Answer::Pass | Answer::NoDecision { .. } => return None,
    };
    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision,
        }
    });
    Some(out.to_string())
}

/// What the phone's Allow tells the assistant.
const ALLOWED: &str = "Allowed on your phone";

/// What a `--check` hook decides: `allow`, `deny` or `ask` (the desk prompt), with the
/// reason; `None` for `Pass`. A held action that ended without an answer never allows: the
/// clock denies, anything else asks at the desk only where a prompt really shows.
fn check_decision(
    assistant: &str,
    permission_mode: &str,
    answer: &Answer,
) -> Option<(&'static str, String)> {
    Some(match answer {
        Answer::Pass => return None,
        Answer::Allow => ("allow", ALLOWED.into()),
        Answer::Deny { message } => ("deny", message.clone()),
        Answer::NoDecision {
            why: Ended::TimedOut,
        } => ("deny", NO_ANSWER.into()),
        Answer::NoDecision { .. } if desk_prompt_shows(assistant, permission_mode) => {
            ("ask", COULDNT_ASK.into())
        }
        Answer::NoDecision { .. } => ("deny", COULDNT_ASK.into()),
    })
}

/// What Claude Code's `PreToolUse` hook (`--check`) prints.
pub fn claude_pre_tool_use(answer: &Answer, permission_mode: &str) -> Option<String> {
    let (decision, reason) = check_decision("claude", permission_mode, answer)?;
    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    });
    Some(out.to_string())
}

/// What Cursor's `beforeShellExecution` and `beforeMCPExecution` hooks (`--check`) print.
pub fn cursor_permission(answer: &Answer) -> Option<String> {
    let (decision, reason) = check_decision("cursor", "", answer)?;
    let out = if decision == "allow" {
        json!({ "permission": decision })
    } else {
        json!({ "permission": decision, "user_message": reason, "agent_message": reason })
    };
    Some(out.to_string())
}
