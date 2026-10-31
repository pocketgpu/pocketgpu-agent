//! Sends job alerts: when a watched job finishes or crashes, every paired phone with a push
//! ticket gets a short push, signed with the agent's key.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use pocketgpu_proto::msg::jobs::JobState;
use pocketgpu_proto::push::{AlertKind, AlertRequest, AlertResult};

use crate::daemon::state::Agent;
use crate::jobs::{JobEnded, store};
use crate::store::devices;

const PATH: &str = "/v1/alerts";
/// Pushes should arrive within 10 seconds of the job ending (F07), so retry briefly.
const RETRIES: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// What a phone sees for a job that ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobText {
    pub title: String,
    pub body: String,
}

/// Push text for an ended job, or `None` when no push is due (alerts off, stopped by hand).
/// Generic text leaves out the job name; log lines are never included.
pub fn job_text(
    job: &JobEnded,
    machine: &str,
    took: Option<Duration>,
    generic: bool,
) -> Option<JobText> {
    if !job.alerts_on {
        return None;
    }
    let took = took.map(fmt_duration);
    let (verb, detail) = match job.state {
        JobState::Finished => (
            "finished",
            took.map_or("Exit 0".into(), |t| format!("Took {t} · exit 0")),
        ),
        JobState::Crashed => {
            let code = job.exit_code.map_or("unknown".into(), |c| c.to_string());
            let after = took.map_or(String::new(), |t| format!(" after {t}"));
            (
                "crashed",
                format!("Exit {code}{after}. Tap to see the last lines"),
            )
        }
        JobState::Lost => (
            "was lost",
            "PocketGPU lost track of it; it may still be running".into(),
        ),
        // Stopped by the user, so they already know.
        JobState::Stopped | JobState::Running | JobState::Unknown => return None,
    };
    Some(if generic {
        JobText {
            title: format!("Job {verb} on {machine}"),
            body: "Open PocketGPU for details".into(),
        }
    } else {
        JobText {
            title: format!("{} {verb} · {machine}", job.name),
            body: format!("Job {} · {detail}", job.id),
        }
    })
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

/// Runs until the agent stops: one push per ended job, to every phone with a ticket.
pub async fn run(agent: Arc<Agent>) {
    let http = match super::client::http() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("alerts are off: {e}");
            return;
        }
    };
    let mut ended = agent.jobs.subscribe();
    loop {
        let job = match ended.recv().await {
            Ok(j) => j,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("skipped {n} job alerts");
                continue;
            }
            Err(_) => return,
        };
        if let Err(e) = send_job_alert(&agent, &http, &job).await {
            tracing::warn!(job = job.id, "job alert not sent: {e:#}");
        }
    }
}

async fn send_job_alert(agent: &Agent, http: &reqwest::Client, job: &JobEnded) -> Result<()> {
    let (enabled, generic, push_url) = {
        let cfg = agent.config.read().expect("config lock poisoned");
        (
            cfg.alerts.job_end,
            cfg.alerts.generic_text,
            cfg.push_url.clone(),
        )
    };
    if !enabled || super::paused_until(agent).is_some() {
        return Ok(());
    }
    let took = store::get(&agent.db(), job.id)?.and_then(|(d, _)| {
        let end = d.summary.ended_ms?;
        Some(Duration::from_millis(
            end.saturating_sub(d.summary.started_ms),
        ))
    });
    let Some(text) = job_text(job, &agent.name(), took, generic) else {
        return Ok(());
    };
    let alert = super::channels::Alert {
        title: text.title.clone(),
        body: text.body.clone(),
        kind: "job_end",
        machine: agent.name(),
        job: Some(job.id),
    };
    super::channels::send_all(agent, &alert).await;
    let tickets = devices::push_tickets(&agent.db())?;
    if tickets.is_empty() {
        return Ok(());
    }
    let alert = AlertRequest {
        tickets,
        kind: AlertKind::JobEnd,
        title: text.title,
        body: text.body,
        link: Some(format!(
            "pocketgpu://machine/{}/job/{}",
            agent.identity.agent_id(),
            job.id
        )),
    };
    let result = post(agent, http, &push_url, &alert).await?;
    tracing::info!(job = job.id, delivered = result.delivered, "job alert sent");
    Ok(())
}

/// A note from `pgpu mcp` (an AI agent on this machine) to every phone and alert channel,
/// sent like a job alert: free on every plan, off when job alerts are off or paused, and
/// generic when the user hides alert text. Returns phones delivered, channels that took it
/// and whether it was held.
pub async fn send_note(agent: &Agent, title: &str, body: &str) -> Result<(u32, u32, bool)> {
    let (enabled, generic, push_url) = {
        let cfg = agent.config.read().expect("config lock poisoned");
        (
            cfg.alerts.job_end,
            cfg.alerts.generic_text,
            cfg.push_url.clone(),
        )
    };
    if !enabled || super::paused_until(agent).is_some() {
        return Ok((0, 0, true));
    }
    let machine = agent.name();
    let (title, body) = note_text(title, body, &machine, generic);
    let alert = super::channels::Alert {
        title: title.clone(),
        body: body.clone(),
        kind: "note",
        machine,
        job: None,
    };
    let channels = super::channels::send_all(agent, &alert)
        .await
        .iter()
        .filter(|(_, err)| err.is_none())
        .count() as u32;
    let tickets = devices::push_tickets(&agent.db())?;
    if tickets.is_empty() {
        return Ok((0, channels, false));
    }
    let http = super::client::http()?;
    let alert = AlertRequest {
        tickets,
        kind: AlertKind::JobEnd,
        title,
        body,
        link: Some(format!("pocketgpu://machine/{}", agent.identity.agent_id())),
    };
    let result = post(agent, &http, &push_url, &alert).await?;
    Ok((result.delivered, channels, false))
}

/// A note's push text: as given, clipped to push limits, or generic when alert text is hidden.
pub fn note_text(title: &str, body: &str, machine: &str, generic: bool) -> (String, String) {
    use pocketgpu_proto::push::{BODY_MAX, TITLE_MAX, clip};
    if generic {
        return (
            format!("Note from {machine}"),
            "Open PocketGPU for details".into(),
        );
    }
    let title = match title.trim() {
        "" => format!("Note from {machine}"),
        t => clip(t, TITLE_MAX),
    };
    (title, clip(body.trim(), BODY_MAX))
}

/// Posts a signed alert, retrying a few times on network errors and server errors.
pub async fn post(
    agent: &Agent,
    http: &reqwest::Client,
    push_url: &str,
    alert: &AlertRequest,
) -> Result<AlertResult> {
    let body = serde_json::to_vec(alert)?;
    let res = super::client::signed_post(agent, http, push_url, PATH, body, &RETRIES).await?;
    res.json()
        .await
        .context("the push server's reply was not valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(state: JobState, exit_code: Option<i32>) -> JobEnded {
        JobEnded {
            id: 12,
            name: "train.py".into(),
            state,
            exit_code,
            alerts_on: true,
        }
    }

    #[test]
    fn detailed_text_names_the_job() {
        let t = job_text(
            &ended(JobState::Finished, Some(0)),
            "rig-1",
            Some(Duration::from_secs(8040)),
            false,
        )
        .unwrap();
        assert_eq!(t.title, "train.py finished · rig-1");
        assert_eq!(t.body, "Job 12 · Took 2h 14m · exit 0");
        let t = job_text(&ended(JobState::Crashed, Some(1)), "rig-1", None, false).unwrap();
        assert!(t.body.starts_with("Job 12 · Exit 1."));
    }

    #[test]
    fn generic_text_hides_the_job() {
        let t = job_text(&ended(JobState::Crashed, Some(1)), "rig-1", None, true).unwrap();
        assert_eq!(t.title, "Job crashed on rig-1");
        assert!(!t.title.contains("train") && !t.body.contains("train"));
    }

    #[test]
    fn notes_are_clipped_or_hidden() {
        let (t, b) = note_text("Tests passed", &"x".repeat(500), "rig-1", false);
        assert_eq!(t, "Tests passed");
        assert_eq!(b.chars().count(), pocketgpu_proto::push::BODY_MAX);
        let (t, _) = note_text("  ", "done", "rig-1", false);
        assert_eq!(t, "Note from rig-1");
        let (t, b) = note_text("Secret project done", "weights uploaded", "rig-1", true);
        assert!(!t.contains("Secret") && !b.contains("weights"));
    }

    #[test]
    fn no_push_when_stopped_or_muted() {
        assert!(job_text(&ended(JobState::Stopped, Some(143)), "rig-1", None, false).is_none());
        let mut muted = ended(JobState::Crashed, Some(1));
        muted.alerts_on = false;
        assert!(job_text(&muted, "rig-1", None, false).is_none());
    }
}
