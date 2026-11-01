//! Requests from the agent to the push server, and how they are signed.
//!
//! The agent signs with its own key (its ID is the public key), so the push server knows
//! which machine sent an alert without any account. Headers: `X-Agent-Id` (hex public key),
//! `X-Timestamp` (Unix seconds) and `X-Signature` (hex ed25519 signature of
//! [`signing_message`]).

use serde::{Deserialize, Serialize};

pub const HEADER_AGENT: &str = "x-agent-id";
pub const HEADER_TIMESTAMP: &str = "x-timestamp";
pub const HEADER_SIGNATURE: &str = "x-signature";
/// Requests older or newer than this are refused, so a captured request can't be replayed later.
pub const MAX_CLOCK_SKEW_S: u64 = 5 * 60;

pub const TITLE_MAX: usize = 60;
pub const BODY_MAX: usize = 140;

/// What is signed: method, path, timestamp and the BLAKE3 hash of the body, one per line.
pub fn signing_message(method: &str, path: &str, timestamp: u64, body: &[u8]) -> Vec<u8> {
    format!(
        "{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        path,
        timestamp,
        blake3::hash(body).to_hex()
    )
    .into_bytes()
}

/// Alert types, matching the alert rules (F08).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// Job finished or crashed: free on every plan.
    JobEnd,
    GpuTemp,
    Offline,
    OllamaDown,
    DiskFull,
}

impl AlertKind {
    /// Only job alerts reach Free tickets.
    pub fn needs_pro(self) -> bool {
        !matches!(self, Self::JobEnd)
    }
}

/// `POST /v1/alerts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRequest {
    pub tickets: Vec<String>,
    pub kind: AlertKind,
    pub title: String,
    pub body: String,
    /// `pocketgpu://machine/{agent_id}/job/{job_id}` or `…/machine/{agent_id}`.
    pub link: Option<String>,
}

/// The reply to `POST /v1/alerts`: how many tickets got the push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertResult {
    pub delivered: u32,
    /// Tickets the server doesn't know (revoked or never registered); the agent may drop them.
    pub unknown: u32,
    /// Non-job alerts not sent because the ticket isn't Pro.
    pub not_entitled: u32,
}

/// `POST /v1/heartbeat`, every 60 seconds. The push server sends "machine offline" to these
/// tickets (Pro only) once heartbeats stop for longer than `offline_after_s`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub offline_after_s: u32,
    pub tickets: Vec<String>,
    /// The agent's version and system ("linux", "windows", "macos"), counted on the admin
    /// page. Agents before them send neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}

pub const HEARTBEAT_EVERY_S: u64 = 60;
/// Shortest offline limit the push server accepts, so a late heartbeat isn't an outage.
pub const MIN_OFFLINE_AFTER_S: u32 = 2 * 60;

/// Cuts text to at most `max` characters, ending with `…` when cut.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_message_covers_every_part() {
        let a = signing_message("post", "/v1/alerts", 1, b"{}");
        assert!(
            String::from_utf8(a.clone())
                .unwrap()
                .starts_with("POST\n/v1/alerts\n1\n")
        );
        assert_ne!(a, signing_message("POST", "/v1/alerts", 2, b"{}"));
        assert_ne!(a, signing_message("POST", "/v1/alerts", 1, b"{ }"));
        assert_ne!(a, signing_message("POST", "/v1/heartbeat", 1, b"{}"));
    }

    #[test]
    fn clips_by_characters() {
        assert_eq!(clip("short", 60), "short");
        assert_eq!(clip("ab😀cd", 4), "ab😀…");
        assert_eq!(clip(&"x".repeat(200), BODY_MAX).chars().count(), BODY_MAX);
    }
}
