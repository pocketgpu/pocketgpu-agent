//! One guest chat: checks, the call to the share's model, and the answer streamed back.
//!
//! The computer writes the request itself from the share: the model is the share's, the
//! answer is capped, and only roles and text come from the guest, so no tools, options or
//! other models. Nothing said is logged, not even its length.

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::SendStream;
use pocketgpu_proto::share::{
    ANSWER_TOKENS, AgentToGuest, ChatMsg, GuestRefusal, MAX_MESSAGE_BYTES, MAX_MESSAGES,
    MAX_TOTAL_BYTES, ModelServerRef, Role,
};
use pocketgpu_proto::write_frame;
use serde_json::{Value, json};

use super::{day, paused, store, store::Share};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

fn http() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        // Only this machine's own servers: no proxy, and no redirect elsewhere.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()
}

/// Whether the server lists the model now (asked briefly).
pub async fn model_available(agent: &Agent, server: ModelServerRef, model: &str) -> bool {
    let (Ok((base, _)), Some(http)) = (crate::tunnel::base_for(agent, server), http()) else {
        return false;
    };
    let path = match server {
        ModelServerRef::Ollama => "/api/tags",
        ModelServerRef::Port(_) => "/v1/models",
    };
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let Ok(Ok(res)) = tokio::time::timeout(Duration::from_secs(3), http.get(url).send()).await
    else {
        return false;
    };
    let Ok(v) = res.json::<Value>().await else {
        return false;
    };
    let names: Vec<&str> = match server {
        ModelServerRef::Ollama => v["models"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["name"].as_str()).collect())
            .unwrap_or_default(),
        ModelServerRef::Port(_) => v["data"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str()).collect())
            .unwrap_or_default(),
    };
    names.contains(&model)
}

/// Checks the messages the page sent with one chat.
pub fn check_messages(messages: &[ChatMsg]) -> Result<(), GuestRefusal> {
    let total: usize = messages.iter().map(|m| m.text.len()).sum();
    let last_is_user = messages.last().is_some_and(|m| m.role == Role::User);
    if messages.is_empty()
        || !last_is_user
        || messages.len() > MAX_MESSAGES
        || total > MAX_TOTAL_BYTES
        || messages.iter().any(|m| m.text.len() > MAX_MESSAGE_BYTES)
    {
        return Err(GuestRefusal::TooLong);
    }
    Ok(())
}

/// The request body for the share's server, written here from the share and the texts.
pub fn body(server: ModelServerRef, model: &str, messages: &[ChatMsg]) -> (String, Value) {
    let msgs: Vec<Value> = messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            json!({ "role": role, "content": m.text })
        })
        .collect();
    match server {
        ModelServerRef::Ollama => (
            "/api/chat".into(),
            json!({ "model": model, "messages": msgs, "stream": true,
                    "options": { "num_predict": ANSWER_TOKENS } }),
        ),
        ModelServerRef::Port(_) => (
            "/v1/chat/completions".into(),
            json!({ "model": model, "messages": msgs, "stream": true,
                    "max_tokens": ANSWER_TOKENS }),
        ),
    }
}

/// The answer's text in one line of the server's stream (Ollama's NDJSON or an
/// OpenAI-style `data:` line); `None` for lines without text.
pub fn text_of(server: ModelServerRef, line: &str) -> Option<String> {
    let line = line.trim();
    let json = match server {
        ModelServerRef::Ollama => line,
        ModelServerRef::Port(_) => line.strip_prefix("data:")?.trim(),
    };
    let v: Value = serde_json::from_str(json).ok()?;
    let text = match server {
        ModelServerRef::Ollama => v["message"]["content"].as_str(),
        ModelServerRef::Port(_) => v["choices"][0]["delta"]["content"].as_str(),
    }?;
    (!text.is_empty()).then(|| text.to_string())
}

async fn send(out: &mut SendStream, msg: &AgentToGuest) -> bool {
    write_frame(out, msg).await.is_ok()
}

/// Runs one chat to the end; refusals are sent as `Refused`.
pub async fn run(agent: &Arc<Agent>, share: &Share, messages: Vec<ChatMsg>, out: &mut SendStream) {
    let refuse = |r| AgentToGuest::Refused(r);
    if let Err(r) = check_messages(&messages) {
        send(out, &refuse(r)).await;
        return;
    }
    let Some(_busy) = agent.shares.begin(share.id) else {
        send(out, &refuse(GuestRefusal::Busy)).await;
        return;
    };
    if paused(agent, share) {
        send(out, &refuse(GuestRefusal::Paused)).await;
        return;
    }
    let now = now_ms();
    let today = day::local_day(now);
    let used = store::used_on(&agent.db(), share.id, &today).unwrap_or(0);
    if share.daily_limit.is_some_and(|l| used >= l) {
        let resets_ms = day::next_local_midnight_ms(now);
        send(out, &refuse(GuestRefusal::LimitReached { resets_ms })).await;
        return;
    }
    let (Ok((base, _)), Some(http)) = (crate::tunnel::base_for(agent, share.server), http()) else {
        send(out, &refuse(GuestRefusal::ModelUnavailable)).await;
        return;
    };
    let (path, json) = body(share.server, &share.model, &messages);
    drop(messages);
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let res = match http.post(url).json(&json).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => {
            send(out, &refuse(GuestRefusal::ModelUnavailable)).await;
            return;
        }
    };
    drop(json);
    // Counted once the model took it, so a model that's gone costs the guest nothing.
    let used = store::count_message(&agent.db(), share.id, &today).unwrap_or(used + 1);
    if stream(share.server, res, out).await {
        send(out, &AgentToGuest::Done { used_today: used }).await;
    }
}

/// Streams the answer's text; false when the guest closed the stream (which drops the
/// request to the server and stops the answer).
async fn stream(server: ModelServerRef, mut res: reqwest::Response, out: &mut SendStream) -> bool {
    let mut pending = Vec::new();
    loop {
        match res.chunk().await {
            Ok(Some(bytes)) => {
                pending.extend_from_slice(&bytes);
                while let Some(i) = pending.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=i).collect();
                    if let Some(t) = text_of(server, &String::from_utf8_lossy(&line))
                        && !send(out, &AgentToGuest::Text(t)).await
                    {
                        return false;
                    }
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    if let Some(t) = text_of(server, &String::from_utf8_lossy(&pending)) {
        return send(out, &AgentToGuest::Text(t)).await;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, text: &str) -> ChatMsg {
        ChatMsg {
            role,
            text: text.into(),
        }
    }

    #[test]
    fn the_model_and_caps_come_from_the_share() {
        let (path, b) = body(ModelServerRef::Ollama, "llama3", &[msg(Role::User, "hi")]);
        assert_eq!(path, "/api/chat");
        assert_eq!(b["model"], "llama3");
        assert_eq!(b["options"]["num_predict"], ANSWER_TOKENS);
        assert_eq!(b["messages"][0], json!({"role": "user", "content": "hi"}));
        let (path, b) = body(ModelServerRef::Port(1234), "qwen", &[msg(Role::User, "hi")]);
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(b["max_tokens"], ANSWER_TOKENS);
        assert!(b.get("tools").is_none());
    }

    #[test]
    fn oversized_or_odd_chats_are_refused() {
        assert!(check_messages(&[msg(Role::User, "hi")]).is_ok());
        assert!(check_messages(&[]).is_err());
        assert!(check_messages(&[msg(Role::Assistant, "hi")]).is_err());
        assert!(check_messages(&[msg(Role::User, &"x".repeat(MAX_MESSAGE_BYTES + 1))]).is_err());
        let many: Vec<_> = (0..=MAX_MESSAGES).map(|_| msg(Role::User, "a")).collect();
        assert!(check_messages(&many).is_err());
        let big: Vec<_> = (0..9).map(|_| msg(Role::User, &"x".repeat(8000))).collect();
        assert!(check_messages(&big).is_err());
    }

    #[test]
    fn text_is_read_from_both_stream_kinds() {
        let o = r#"{"message":{"role":"assistant","content":"Hel"},"done":false}"#;
        assert_eq!(text_of(ModelServerRef::Ollama, o).as_deref(), Some("Hel"));
        let s = r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#;
        assert_eq!(text_of(ModelServerRef::Port(1), s).as_deref(), Some("lo"));
        assert_eq!(text_of(ModelServerRef::Port(1), "data: [DONE]"), None);
        assert_eq!(text_of(ModelServerRef::Ollama, r#"{"done":true}"#), None);
    }
}
