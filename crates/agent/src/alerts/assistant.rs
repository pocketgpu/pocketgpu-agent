//! Sends AI assistant alerts (F32): notes from `pgpu mcp`, and "needs you" and "finished"
//! from assistants' hooks. Free on every plan; off when the user turns AI assistant alerts
//! off, held while alerts are paused, and only for the owner's own phones: a guest (F44)
//! never gets them, by push or over `pgpu/events/1`. Approvals (F32) go only to the owner's
//! Full control devices: [`super::approval`] for the push, [`event`] for open apps.

use anyhow::Result;
use pocketgpu_proto::push::{AlertKind, AlertRequest, AlertResult};

use crate::daemon::state::Agent;
use crate::store::devices;

/// The kind alert channels and open apps see for a note from `pgpu mcp`.
pub const NOTE: &str = "note";
/// The kind they see for an alert from an assistant's hook.
pub const ASSISTANT: &str = "assistant";

/// An approval push (F32): owner Full control devices only.
pub const APPROVAL: &str = "approval";
/// Live updates for open apps about held actions and their settings (F32); the request id
/// rides in the alert's `job` field, since `events::Alert` has no link.
pub const APPROVAL_OPENED: &str = "approval_opened";
pub const APPROVAL_ENDED: &str = "approval_ended";
pub const SETTINGS: &str = "assistant_settings";

/// Kinds a guest's open app never hears over `pgpu/events/1`.
pub fn owner_only(kind: &str) -> bool {
    matches!(kind, NOTE | ASSISTANT) || shell_only(kind)
}

/// Kinds only the owner's Full control devices hear: what an assistant waits for is as much
/// as a terminal.
pub fn shell_only(kind: &str) -> bool {
    matches!(kind, APPROVAL | APPROVAL_OPENED | APPROVAL_ENDED | SETTINGS)
}

/// Tells open apps (not push, not alert channels) that a request opened or ended, or that
/// the settings changed. Never the command: only the assistant and the computer.
pub fn event(agent: &Agent, kind: &'static str, title: String, id: Option<u64>) {
    debug_assert!(shell_only(kind));
    // Nobody listening is not an error.
    let _ = agent.events.send(pocketgpu_proto::msg::events::Alert {
        title,
        body: String::new(),
        kind: kind.to_string(),
        job: id,
        at_ms: crate::store::db::now_ms() as u64,
    });
}

/// How a send went: phones the push server delivered to, channels that took it, and
/// whether it was held (assistant alerts off, or alerts paused).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    pub phones: u32,
    pub channels: u32,
    pub held: bool,
}

/// Sends to the alert channels and the owner's phones. `channel_kind` is [`NOTE`] or
/// [`ASSISTANT`].
pub async fn send(
    agent: &Agent,
    title: String,
    body: String,
    channel_kind: &'static str,
) -> Result<Sent> {
    let (enabled, push_url) = {
        let cfg = agent.config.read().expect("config lock poisoned");
        (cfg.alerts.assistant, cfg.push_url.clone())
    };
    if !enabled || super::paused_until(agent).is_some() {
        return Ok(Sent {
            phones: 0,
            channels: 0,
            held: true,
        });
    }
    let alert = super::channels::Alert {
        title: title.clone(),
        body: body.clone(),
        kind: channel_kind,
        machine: agent.name(),
        job: None,
    };
    let channels = super::channels::send_all(agent, &alert)
        .await
        .iter()
        .filter(|(_, err)| err.is_none())
        .count() as u32;
    let tickets = devices::owner_push_tickets(&agent.db())?;
    if tickets.is_empty() {
        return Ok(Sent {
            phones: 0,
            channels,
            held: false,
        });
    }
    let http = super::client::http()?;
    let alert = AlertRequest {
        tickets,
        kind: AlertKind::Assistant,
        title,
        body,
        link: Some(format!("pocketgpu://machine/{}", agent.identity.agent_id())),
    };
    let result = post(agent, &http, &push_url, alert).await?;
    Ok(Sent {
        phones: result.delivered,
        channels,
        held: false,
    })
}

/// Posts an assistant alert. A push server from before assistant alerts doesn't know the
/// kind and answers 400; it gets the same alert once more as a job alert, which it delivers
/// on every plan too.
pub async fn post(
    agent: &Agent,
    http: &reqwest::Client,
    push_url: &str,
    mut alert: AlertRequest,
) -> Result<AlertResult> {
    match super::sender::post(agent, http, push_url, &alert).await {
        Err(e) if alert.kind == AlertKind::Assistant && unknown_kind(&e) => {
            tracing::info!("the push server doesn't know assistant alerts; sent as a job alert");
            alert.kind = AlertKind::JobEnd;
            super::sender::post(agent, http, push_url, &alert).await
        }
        other => other,
    }
}

fn unknown_kind(e: &anyhow::Error) -> bool {
    e.downcast_ref::<super::client::Refused>()
        .is_some_and(|r| r.status == reqwest::StatusCode::BAD_REQUEST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_400_means_an_older_server() {
        let refused = |status| -> anyhow::Error {
            super::super::client::Refused {
                path: "/v1/alerts".into(),
                status,
            }
            .into()
        };
        assert!(unknown_kind(&refused(reqwest::StatusCode::BAD_REQUEST)));
        assert!(!unknown_kind(&refused(reqwest::StatusCode::UNAUTHORIZED)));
        assert!(!unknown_kind(&refused(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        )));
        assert!(!unknown_kind(&anyhow::anyhow!("push server unreachable")));
        // Wrapped with context, it is still found.
        let wrapped = refused(reqwest::StatusCode::BAD_REQUEST).context("sending");
        assert!(unknown_kind(&wrapped));
    }

    #[test]
    fn notes_and_assistant_alerts_stay_with_the_owner() {
        assert!(owner_only(NOTE) && owner_only(ASSISTANT));
        assert!(!owner_only("job_end") && !owner_only("gpu_temp"));
        for kind in [APPROVAL, APPROVAL_OPENED, APPROVAL_ENDED, SETTINGS] {
            assert!(owner_only(kind) && shell_only(kind), "{kind}");
        }
        assert!(!shell_only(NOTE) && !shell_only(ASSISTANT));
    }
}
