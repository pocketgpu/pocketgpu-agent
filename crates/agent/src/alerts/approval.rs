//! The approval push (F32): an AI assistant waits for an answer on the phone. The Pro kind
//! `Approval`, so the push server applies the plan; only the owner's Full control devices,
//! since only they may answer; never the command, a file or a folder. No alert channels and
//! no fallback to another kind: a push that reaches no phone ends the request at once as
//! "no phone reached" (the caller does that), and the assistant asks at the desk.

use anyhow::Result;
use pocketgpu_proto::assistants::ApprovalView;
use pocketgpu_proto::push::{AlertKind, AlertRequest, BODY_MAX, TITLE_MAX, clip};

use crate::daemon::state::Agent;
use crate::store::devices;

/// The push's body: the same for every request.
pub const BODY: &str = "Tap to see what it wants and answer.";

/// The title and body: the assistant and the computer, nothing it wrote.
pub fn text(assistant: &str, machine: &str) -> (String, String) {
    let name = crate::assistants::display_name(assistant);
    (
        clip(&format!("{name} asks you · {machine}"), TITLE_MAX),
        clip(BODY, BODY_MAX),
    )
}

/// Where the push opens: the request on this computer.
pub fn link(agent_id: &str, request: u64) -> String {
    format!("pocketgpu://machine/{agent_id}/assistants/{request}")
}

/// Sends the push for `view` and says how many phones got it. 0 when alerts are paused,
/// no Full control device has a ticket, the hourly limit is reached, or the push server
/// refused or couldn't be reached: never an error the caller has to tell apart.
pub async fn send(agent: &Agent, view: &ApprovalView) -> u32 {
    match try_send(agent, view).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(id = view.id, "approval push not sent: {e:#}");
            0
        }
    }
}

async fn try_send(agent: &Agent, view: &ApprovalView) -> Result<u32> {
    if super::paused_until(agent).is_some() {
        return Ok(0);
    }
    // End-to-end tests and simulators can't get real pushes (`PGPU_FAKE_PUSH`): act as if one
    // phone got it, so the request waits for an answer from the app.
    if std::env::var_os("PGPU_FAKE_PUSH").is_some() {
        tracing::info!(id = view.id, "approval push faked (PGPU_FAKE_PUSH)");
        return Ok(1);
    }
    let tickets = devices::owner_shell_push_tickets(&agent.db())?;
    if tickets.is_empty() {
        return Ok(0);
    }
    let now = crate::store::db::now_ms();
    if !agent.assistants.allow_push(&view.assistant, now) {
        tracing::info!(id = view.id, "approval push over the hourly limit");
        return Ok(0);
    }
    let push_url = agent
        .config
        .read()
        .expect("config lock poisoned")
        .push_url
        .clone();
    let (title, body) = text(&view.assistant, &agent.name());
    let alert = AlertRequest {
        tickets,
        kind: AlertKind::Approval,
        title,
        body,
        link: Some(link(&agent.identity.agent_id().to_string(), view.id)),
    };
    let http = super::client::http()?;
    let result = super::sender::post(agent, &http, &push_url, &alert).await?;
    Ok(result.delivered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_push_names_the_assistant_and_computer_only() {
        let (t, b) = text("claude", "rig-01");
        assert_eq!(t, "Claude Code asks you · rig-01");
        assert_eq!(b, "Tap to see what it wants and answer.");
        let (t, _) = text("claude", &"m".repeat(200));
        assert_eq!(t.chars().count(), TITLE_MAX);
        assert_eq!(
            link(&"a".repeat(64), 7),
            format!("pocketgpu://machine/{}/assistants/7", "a".repeat(64))
        );
    }
}
