//! Alert channels (F20): ntfy, Telegram, Discord and webhooks, sent straight from the agent.
//!
//! They get the same alerts and the same text as the push server (generic text included), and
//! work without our servers. Offline alerts can't use them: a machine that's off can't send.
//! The settings live in `config.toml`, which is private to the user because it holds tokens.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use pocketgpu_proto::local::Channel;
use serde_json::json;

use crate::daemon::state::Agent;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Channels per machine.
pub const MAX: usize = 10;

/// What every channel receives.
#[derive(Debug, Clone)]
pub struct Alert {
    pub title: String,
    pub body: String,
    /// `job_end`, `gpu_temp`, `disk_full`, `ollama_down` or `test`.
    pub kind: &'static str,
    pub machine: String,
    /// The job, for job alerts.
    pub job: Option<u64>,
}

/// Checks a channel before it is saved.
pub fn validate(c: &Channel) -> Result<()> {
    let url = |u: &str, what: &str| -> Result<()> {
        if !(u.starts_with("https://") || u.starts_with("http://")) || u.len() > 500 {
            bail!("{what} must be an http:// or https:// URL");
        }
        Ok(())
    };
    match c {
        Channel::Ntfy { topic } => {
            if topic.contains("://") {
                url(topic, "the ntfy topic URL")?;
            } else if topic.is_empty()
                || topic.len() > 64
                || !topic
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            {
                bail!("an ntfy topic uses letters, digits, - and _ (up to 64)");
            }
        }
        Channel::Telegram { bot_token, chat_id } => {
            let (id, secret) = bot_token.split_once(':').unwrap_or(("", ""));
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) || secret.len() < 20 {
                bail!("that doesn't look like a bot token from @BotFather (123456:ABC…)");
            }
            let digits = chat_id.strip_prefix('-').unwrap_or(chat_id);
            if !(digits.chars().all(|c| c.is_ascii_digit()) && !digits.is_empty()
                || chat_id.starts_with('@'))
            {
                bail!("the chat ID is a number (or @channelname)");
            }
        }
        Channel::Discord { url: u } => {
            if !(u.starts_with("https://discord.com/api/webhooks/")
                || u.starts_with("https://discordapp.com/api/webhooks/"))
            {
                bail!("use the webhook URL from Discord: https://discord.com/api/webhooks/…");
            }
        }
        Channel::Webhook { url: u } => url(u, "the webhook")?,
    }
    Ok(())
}

/// A line for `pgpu notify`, with tokens and webhook secrets masked.
pub fn describe(c: &Channel) -> String {
    let mask = |s: &str| format!("{}…", s.chars().take(8).collect::<String>());
    match c {
        Channel::Ntfy { topic } => format!("ntfy · {topic}"),
        Channel::Telegram { bot_token, chat_id } => {
            format!("telegram · chat {chat_id} · bot {}", mask(bot_token))
        }
        Channel::Discord { url } => {
            let id = url.trim_end_matches('/').rsplit('/').nth(1).unwrap_or("");
            format!("discord · webhook {}", mask(id))
        }
        Channel::Webhook { url } => {
            let host = url
                .split("://")
                .nth(1)
                .and_then(|r| r.split('/').next())
                .unwrap_or(url);
            format!("webhook · {host}")
        }
    }
}

/// Sends `alert` to every channel; returns each channel's result.
pub async fn send_all(agent: &Agent, alert: &Alert) -> Vec<(String, Option<String>)> {
    // Apps that are open hear it too; nobody listening is not an error.
    let _ = agent.events.send(pocketgpu_proto::msg::events::Alert {
        title: alert.title.clone(),
        body: alert.body.clone(),
        kind: alert.kind.to_string(),
        job: alert.job,
        at_ms: crate::store::db::now_ms() as u64,
    });
    let channels = agent
        .config
        .read()
        .expect("config lock poisoned")
        .notify
        .clone();
    if channels.is_empty() {
        return Vec::new();
    }
    // No redirects: a channel answers at its own address or not at all.
    let http = match reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(h) => h,
        Err(e) => {
            return channels
                .iter()
                .map(|c| (describe(c), Some(e.to_string())))
                .collect();
        }
    };
    let mut out = Vec::new();
    for c in &channels {
        let result = send(&http, c, alert).await;
        if let Err(e) = &result {
            tracing::warn!(channel = %describe(c), "alert channel failed: {e:#}");
        }
        out.push((describe(c), result.err().map(|e| format!("{e:#}"))));
    }
    out
}

async fn send(http: &reqwest::Client, c: &Channel, a: &Alert) -> Result<()> {
    let req = match c {
        Channel::Ntfy { topic } => {
            let url = if topic.contains("://") {
                topic.clone()
            } else {
                format!("https://ntfy.sh/{topic}")
            };
            http.post(url)
                .header("Title", header_safe(&a.title))
                .header("Tags", "pocketgpu")
                .body(a.body.clone())
        }
        Channel::Telegram { bot_token, chat_id } => http
            .post(format!(
                "https://api.telegram.org/bot{bot_token}/sendMessage"
            ))
            .json(&json!({ "chat_id": chat_id, "text": format!("{}\n{}", a.title, a.body) })),
        Channel::Discord { url } => http
            .post(url)
            .json(&json!({ "content": format!("**{}**\n{}", a.title, a.body) })),
        Channel::Webhook { url } => http.post(url).json(&json!({
            "title": a.title, "body": a.body, "kind": a.kind, "machine": a.machine,
        })),
    };
    // Without the URL: Telegram's holds the bot token, Discord's and webhooks' are secrets.
    let res = req
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .context("could not reach it")?;
    if !res.status().is_success() {
        bail!("it answered {}", res.status());
    }
    Ok(())
}

/// HTTP headers can't hold line breaks or non-ASCII; ntfy titles are short anyway.
fn header_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_targets() {
        assert!(
            validate(&Channel::Ntfy {
                topic: "rig1-alerts".into()
            })
            .is_ok()
        );
        assert!(
            validate(&Channel::Ntfy {
                topic: "has space".into()
            })
            .is_err()
        );
        assert!(
            validate(&Channel::Ntfy {
                topic: "https://ntfy.example.com/rig".into()
            })
            .is_ok()
        );
        let tg = |token: &str, chat: &str| Channel::Telegram {
            bot_token: token.into(),
            chat_id: chat.into(),
        };
        assert!(validate(&tg("123456:ABCdefGhIJKlmNoPQRsTUVwxyZ0123", "-1001234")).is_ok());
        assert!(validate(&tg("abc", "1")).is_err());
        assert!(validate(&tg("123456:ABCdefGhIJKlmNoPQRsTUVwxyZ0123", "chat")).is_err());
        assert!(
            validate(&Channel::Discord {
                url: "https://example.com/hook".into()
            })
            .is_err()
        );
        assert!(
            validate(&Channel::Webhook {
                url: "ftp://x".into()
            })
            .is_err()
        );
    }

    #[test]
    fn hides_secrets() {
        let tg = Channel::Telegram {
            bot_token: "123456:ABCdefGhIJKlmNoPQRsTUVwxyZ0123".into(),
            chat_id: "42".into(),
        };
        assert_eq!(describe(&tg), "telegram · chat 42 · bot 123456:A…");
        let d = Channel::Discord {
            url: "https://discord.com/api/webhooks/1234567890/secret-token".into(),
        };
        assert_eq!(describe(&d), "discord · webhook 12345678…");
        assert!(!describe(&d).contains("secret"));
        let w = Channel::Webhook {
            url: "https://hooks.example.com/path?key=secret".into(),
        };
        assert_eq!(describe(&w), "webhook · hooks.example.com");
        assert_eq!(
            header_safe("Training finished · rig-1"),
            "Training finished   rig-1"
        );
    }
}
