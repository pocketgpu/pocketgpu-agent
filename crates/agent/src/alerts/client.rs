//! Signed requests from the agent to the push server (`pocketgpu_proto::push`).

use std::time::Duration;

use anyhow::{Result, bail};
use pocketgpu_proto::push::{HEADER_AGENT, HEADER_SIGNATURE, HEADER_TIMESTAMP, signing_message};

use crate::daemon::state::Agent;

/// POSTs `body` to `path`, signed with the agent's key. Retries after each wait in
/// `retries` on network and server errors; a refusal (4xx) is final.
pub async fn signed_post(
    agent: &Agent,
    http: &reqwest::Client,
    push_url: &str,
    path: &str,
    body: Vec<u8>,
    retries: &[Duration],
) -> Result<reqwest::Response> {
    let url = format!("{}{path}", push_url.trim_end_matches('/'));
    let mut last = None;
    for wait in std::iter::once(&Duration::ZERO).chain(retries) {
        tokio::time::sleep(*wait).await;
        let ts = crate::store::db::now_ms() as u64 / 1000;
        let sig = agent
            .identity
            .secret
            .sign(&signing_message("POST", path, ts, &body));
        let res = http
            .post(&url)
            .header("content-type", "application/json")
            .header(HEADER_AGENT, agent.identity.agent_id().to_string())
            .header(HEADER_TIMESTAMP, ts.to_string())
            .header(HEADER_SIGNATURE, hex(&sig.to_bytes()))
            .body(body.clone())
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => return Ok(r),
            // Refused for a reason retrying won't fix (signature, limit, bad request).
            Ok(r) if r.status().is_client_error() => {
                bail!("the push server refused {path}: {}", r.status())
            }
            Ok(r) => last = Some(anyhow::anyhow!("push server error {}", r.status())),
            Err(e) => {
                last = Some(anyhow::anyhow!(
                    "push server unreachable: {}",
                    e.without_url()
                ))
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no attempt made")))
}

pub fn http() -> Result<reqwest::Client> {
    // No redirects: a push server that moves gets a new address in the config, and a
    // compromised one can't send every agent's signed requests elsewhere.
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
