//! AI assistants on this computer (F32): their hooks (`pgpu hook <assistant>`) tell the agent
//! when a turn starts, when one asks the user something and when it ends. The rules live
//! here, in the agent, so the hook lines and plugins stay thin. A push says only which
//! assistant, which computer and how long the turn took: never anything the assistant wrote.

pub mod answers;
pub mod approvals;
pub mod blocked;
pub mod guard;
pub mod hold;
pub mod kinds;
pub mod limits;
pub mod secrets;
pub mod seen;
pub mod settings;
pub mod tools;
pub mod tracker;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pocketgpu_proto::local::AssistantEventKind;
use pocketgpu_proto::push::{TITLE_MAX, clip};

use crate::daemon::state::Agent;
use crate::store::db::now_ms;
use tracker::{Alert, Decision, NEEDS_YOU_WAIT_MS, Tracker};

/// The longest assistant name kept, and the longest session id.
const NAME_MAX: usize = 24;
const SESSION_MAX: usize = 128;

/// The running agent's view of its assistants' sessions, in memory only.
#[derive(Debug, Default)]
pub struct Assistants {
    tracker: Mutex<Tracker>,
    /// Held actions waiting for the phone (F32 approvals).
    pub approvals: approvals::Approvals,
    /// When each assistant was last heard from (the AI assistants screen).
    pub seen: seen::Seen,
}

impl Assistants {
    fn tracker(&self) -> std::sync::MutexGuard<'_, Tracker> {
        self.tracker.lock().expect("assistant lock poisoned")
    }

    /// Whether a note from `pgpu mcp` may go out now, counting it if so. One count for the
    /// whole agent: every assistant session runs its own `pgpu mcp`.
    pub fn allow_note(&self, now: i64) -> bool {
        self.tracker().allow_note(now)
    }

    /// Whether an approval push may go out now, counting it if so (the same hourly limits
    /// as the alerts).
    pub fn allow_push(&self, assistant: &str, now: i64) -> bool {
        self.tracker().allow_push(assistant, now)
    }
}

/// `AssistantEvent` from `pgpu hook`: decides at once and sends later, so the hook never
/// waits on the push server. Refused only for a name that isn't one.
pub fn event(
    agent: &Arc<Agent>,
    assistant: &str,
    session: &str,
    event: AssistantEventKind,
) -> Result<(), String> {
    let Some(name) = clean_name(assistant) else {
        return Err("give the assistant's short name, such as claude, codex or cursor".into());
    };
    let key = name.to_ascii_lowercase();
    let session = clean_session(session);
    agent
        .assistants
        .seen
        .touch(&key, seen::Hook::Alerts, now_ms());
    // The desk may have answered a held "Ask on my phone" request in this session.
    let desk = match event {
        AssistantEventKind::ToolDone { input_hash } => Some(approvals::Desk::ToolDone(input_hash)),
        AssistantEventKind::TurnStarted | AssistantEventKind::TurnEnded => {
            Some(approvals::Desk::TurnOver)
        }
        AssistantEventKind::NeedsYou | AssistantEventKind::Idle | AssistantEventKind::Working => {
            None
        }
    };
    if let Some(what) = desk {
        hold::desk(agent, &key, &session, what);
    }
    let decision = agent
        .assistants
        .tracker()
        .event(&key, &session, event, now_ms());
    match decision {
        Decision::Nothing => {}
        Decision::Send(alert) => {
            let agent = agent.clone();
            tokio::spawn(async move { send(&agent, &name, alert).await });
        }
        Decision::Wait { mark } => {
            let agent = agent.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(NEEDS_YOU_WAIT_MS as u64)).await;
                let alert = agent
                    .assistants
                    .tracker()
                    .wait_over(&key, &session, mark, now_ms());
                // The phone was asked already, with its own push.
                let asked = agent.assistants.approvals.waiting_in(&key, &session);
                if let Some(alert) = alert.filter(|_| !asked) {
                    send(&agent, &name, alert).await;
                }
            });
        }
    }
    Ok(())
}

async fn send(agent: &Agent, name: &str, alert: Alert) {
    let (title, body) = text(alert, name, &agent.name());
    match crate::alerts::assistant::send(agent, title, body, crate::alerts::assistant::ASSISTANT)
        .await
    {
        Ok(s) if s.held => tracing::info!(assistant = name, "assistant alert held"),
        Ok(s) => tracing::info!(
            assistant = name,
            phones = s.phones,
            channels = s.channels,
            "assistant alert sent"
        ),
        Err(e) => tracing::warn!(assistant = name, "assistant alert not sent: {e:#}"),
    }
}

/// The push for an alert: the assistant's name and the machine, nothing it wrote.
pub fn text(alert: Alert, name: &str, machine: &str) -> (String, String) {
    let name = display_name(name);
    match alert {
        Alert::NeedsYou => (
            clip(&format!("{name} needs you · {machine}"), TITLE_MAX),
            "Waiting for your answer at the computer".into(),
        ),
        Alert::Finished { minutes } => (
            clip(&format!("{name} finished · {machine}"), TITLE_MAX),
            format!("After {minutes} min. Tap to see the computer."),
        ),
    }
}

/// How people know the assistant: its product name, or the short name with a capital.
pub fn display_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claudecode" => "Claude Code".into(),
        "codex" => "Codex".into(),
        "cursor" => "Cursor".into(),
        _ => {
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

/// A short name of letters, digits, `-`, `_` and `.`; `None` when nothing is left.
fn clean_name(name: &str) -> Option<String> {
    let name: String = name
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(NAME_MAX)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Session ids are only compared, never shown or sent; cut to a sane length.
fn clean_session(session: &str) -> String {
    match session.trim() {
        "" => "-".into(),
        s => s.chars().take(SESSION_MAX).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_name_the_assistant_and_machine_only() {
        let (t, b) = text(Alert::NeedsYou, "claude", "rig-01");
        assert_eq!(t, "Claude Code needs you · rig-01");
        assert_eq!(b, "Waiting for your answer at the computer");
        let (t, b) = text(Alert::Finished { minutes: 40 }, "codex", "rig-01");
        assert_eq!(t, "Codex finished · rig-01");
        assert_eq!(b, "After 40 min. Tap to see the computer.");
        let (t, _) = text(Alert::NeedsYou, "Cursor", "lab");
        assert_eq!(t, "Cursor needs you · lab");
        let (t, _) = text(Alert::Finished { minutes: 12 }, "gemini", "lab");
        assert_eq!(t, "Gemini finished · lab");
        let (t, _) = text(Alert::NeedsYou, "x", &"m".repeat(200));
        assert_eq!(t.chars().count(), TITLE_MAX);
    }

    #[test]
    fn names_and_sessions_are_cleaned() {
        assert_eq!(clean_name(" claude "), Some("claude".into()));
        assert_eq!(clean_name("my agent!"), Some("myagent".into()));
        assert_eq!(
            clean_name("a".repeat(100).as_str()).unwrap().len(),
            NAME_MAX
        );
        assert_eq!(clean_name(" !? "), None);
        assert_eq!(clean_session(""), "-");
        assert_eq!(clean_session(&"s".repeat(500)).len(), SESSION_MAX);
        assert_eq!(display_name("Claude-Code"), "Claude Code");
        assert_eq!(display_name("aider"), "Aider");
    }
}
