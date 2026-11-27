//! The running agent's side of held actions (F32): decides at once whether a hook's call
//! is held, starts the 5-minute timer and the push, checks the answering phone again, and
//! writes the audit when a request ends. The rules themselves are in [`super::approvals`].

use std::sync::Arc;
use std::time::Duration;

use pocketgpu_proto::Permission;
use pocketgpu_proto::assistants::{Answer, ApprovalView, AskWhy, DETAIL_MAX, Ended, Kind};

use super::approvals::{self, Desk, Done, Held, NewRequest, Opened};
use super::{clean_name, clean_session, kinds, secrets};
use crate::daemon::state::Agent;
use crate::store::config::AssistantConfig;
use crate::store::db::now_ms;

/// The first reply to a hook: an answer now, or held (the answer comes on `rx`).
#[derive(Debug)]
pub enum First {
    Answer(Answer),
    Held(Held),
}

/// A hook's call, from `AssistantCheck` (`why: AskFirst`) or `AssistantAsk`.
#[derive(Debug, Clone)]
pub struct Call {
    pub why: AskWhy,
    pub assistant: String,
    pub session: String,
    pub permission_mode: String,
    pub tool: String,
    pub input: String,
    pub oversize: bool,
    pub folder: String,
}

/// Longest folder and tool name kept. The hook cuts the tool name the same way before it
/// hashes a finished call for desk detection.
const FOLDER_MAX: usize = 512;
pub const TOOL_MAX: usize = 64;

/// Decides without waiting: `Pass`, an answer at once, or held.
pub fn start(agent: &Arc<Agent>, call: Call) -> First {
    let cfg = agent
        .config
        .read()
        .expect("config lock poisoned")
        .assistants
        .clone();
    let assistant = clean_name(&call.assistant)
        .unwrap_or_else(|| "unknown".into())
        .to_ascii_lowercase();
    let session = clean_session(&call.session);
    let hook = match call.why {
        AskWhy::AskFirst => super::seen::Hook::Check,
        AskWhy::AskOnPhone => super::seen::Hook::Ask,
    };
    agent.assistants.seen.touch(&assistant, hook, now_ms());
    let tool: String = call.tool.chars().take(TOOL_MAX).collect();
    let hash = approvals::input_hash(&tool, &call.input);
    let detail_text = kinds::command_text(&tool, &call.input);
    let kind = match call.why {
        AskWhy::AskFirst => {
            // Each run needs its own Allow: an earlier one's goes, whatever happens next.
            agent
                .assistants
                .approvals
                .forget(&assistant, &session, &hash);
            if !cfg.armed() {
                return First::Answer(Answer::Pass);
            }
            let kind = if call.oversize {
                Some(Kind::Unknown)
            } else {
                matched(&cfg, &tool, &call.input)
            };
            let Some(kind) = kind else {
                return First::Answer(Answer::Pass);
            };
            // A held check has no desk prompt, so what the phone can't show whole isn't held.
            if call.oversize || detail_text.len() > DETAIL_MAX {
                return First::Answer(approvals::fail_closed(
                    &assistant,
                    &call.permission_mode,
                    Ended::NoPhone,
                ));
            }
            kind
        }
        AskWhy::AskOnPhone => {
            // The desk prompt after this run's "Ask me first" Allow (a user's ask rule):
            // the phone already answered it, whatever "Ask on my phone" says.
            let remembered =
                agent
                    .assistants
                    .approvals
                    .take_remembered(&assistant, &session, &hash, now_ms());
            if remembered {
                return First::Answer(Answer::Allow);
            }
            if !cfg.ask_on_phone {
                return First::Answer(Answer::Pass);
            }
            if call.oversize {
                Kind::Unknown
            } else {
                kinds::classify(&tool, &call.input)
            }
        }
    };
    let new = NewRequest {
        why: call.why,
        kind,
        summary: secrets::summary(&detail_text),
        assistant,
        session,
        permission_mode: call.permission_mode,
        tool,
        detail_text,
        input_hash: hash,
        folder: call.folder.chars().take(FOLDER_MAX).collect(),
    };
    match agent.assistants.approvals.open(new, now_ms()) {
        Opened::Ended(answer, done) => {
            audit(agent, Some(done));
            First::Answer(answer)
        }
        Opened::Held(held) => {
            if !held.joined {
                opened_event(agent, held.id);
                watch(agent, held.id);
            }
            First::Held(held)
        }
    }
}

fn matched(cfg: &AssistantConfig, tool: &str, input: &str) -> Option<Kind> {
    kinds::ask_first(tool, input, &cfg.ask_first_kinds(), &cfg.own_words)
}

/// The timer and the push for a new request.
fn watch(agent: &Arc<Agent>, id: u64) {
    let timer = agent.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(
            pocketgpu_proto::assistants::WAIT_MS as u64,
        ))
        .await;
        end(&timer, id, Ended::TimedOut);
    });
    let push = agent.clone();
    tokio::spawn(async move {
        let Some(view) = push.assistants.approvals.get(id) else {
            return;
        };
        // Reaching no phone ends it at once: the desk prompt or the fail-closed answer,
        // never an allow.
        if crate::alerts::approval::send(&push, &view).await == 0 {
            end(&push, id, Ended::NoPhone);
        }
    });
}

/// Open apps of Full control devices hear of a new request (never its command).
fn opened_event(agent: &Agent, id: u64) {
    let Some(view) = agent.assistants.approvals.get(id) else {
        return;
    };
    let (title, _) = crate::alerts::approval::text(&view.assistant, &agent.name());
    crate::alerts::assistant::event(
        agent,
        crate::alerts::assistant::APPROVAL_OPENED,
        title,
        Some(id),
    );
}

/// Ends an open request without a phone answer (the clock, no phone) and audits it.
pub fn end(agent: &Agent, id: u64, why: Ended) {
    let done = agent.assistants.approvals.end(id, why, now_ms());
    audit(agent, done);
}

/// The hook's stream closed while it waited.
pub fn leave(agent: &Agent, id: u64, waiter: u64) {
    let done = agent.assistants.approvals.leave(id, waiter, now_ms());
    audit(agent, done);
}

/// The desk answered in this session (`PostToolUse(Failure)`, `Stop`, the next prompt).
pub fn desk(agent: &Agent, assistant: &str, session: &str, what: Desk) {
    let assistant = clean_name(assistant)
        .unwrap_or_else(|| "unknown".into())
        .to_ascii_lowercase();
    let session = clean_session(session);
    let ended = agent
        .assistants
        .approvals
        .desk(&assistant, &session, what, now_ms());
    for done in ended {
        audit(agent, Some(done));
    }
}

/// A phone's Allow or Deny. The device is read again here: still paired, for good (not a
/// guest), with Full control.
pub fn answer(
    agent: &Agent,
    device: &str,
    id: u64,
    opened_ms: i64,
    allow: bool,
) -> Result<ApprovalView, String> {
    let name = {
        let db = agent.db();
        let dev = crate::store::devices::get(&db, device)
            .map_err(|e| format!("{e:#}"))?
            .ok_or("this device is no longer paired")?;
        if dev.expires_ms.is_some() {
            return Err("guests can't answer AI assistants".into());
        }
        if dev.permission.parse::<Permission>() != Ok(Permission::Shell) {
            return Err("answering AI assistants needs Full control".into());
        }
        dev.name
    };
    let (view, done) =
        agent
            .assistants
            .approvals
            .answer(id, opened_ms, device, &name, allow, now_ms())?;
    audit(agent, Some(done));
    Ok(view)
}

/// `[assistants]` changed: saved, the guard file rewritten, audited as old -> new.
/// `device` is the phone's key or `local`. More of "Ask me first" is refused while an
/// assistant here can't keep it ([`super::blocked`]).
pub fn save_settings(agent: &Agent, next: AssistantConfig, device: &str) -> Result<(), String> {
    next.validate()?;
    let blocked = super::blocked::read(&agent.paths.data_dir);
    let old = {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        super::blocked::refuse_arming(&blocked, &cfg.assistants, &next)?;
        let mut new_cfg = cfg.clone();
        let old = std::mem::replace(&mut new_cfg.assistants, next.clone());
        new_cfg
            .save(&agent.paths.config_file())
            .map_err(|e| format!("{e:#}"))?;
        *cfg = new_cfg;
        old
    };
    super::guard::write(&agent.paths, next.armed()).map_err(|e| format!("{e:#}"))?;
    crate::alerts::assistant::event(
        agent,
        crate::alerts::assistant::SETTINGS,
        format!("AI assistant settings changed · {}", agent.name()),
        None,
    );
    let _ = crate::audit::record(
        &agent.db(),
        device,
        crate::audit::Event::AssistantSettings,
        "assistants",
        &format!("{} -> {}", describe(&old), describe(&next)),
    );
    Ok(())
}

/// Settings in a few words for the audit.
fn describe(c: &AssistantConfig) -> String {
    let first = if c.ask_first.is_empty() {
        "none".into()
    } else {
        c.ask_first.join(",")
    };
    let mut text = format!(
        "ask on phone {}, ask first {first}, {} own words",
        if c.ask_on_phone { "on" } else { "off" },
        c.own_words.len()
    );
    // Levels above View, which is the default (US4).
    let raised: Vec<String> = c
        .levels
        .keys()
        .filter(|id| c.level(id) != pocketgpu_proto::Permission::View)
        .map(|id| format!("{id}={}", c.level(id).as_str()))
        .collect();
    if !raised.is_empty() {
        text.push_str(&format!(", levels {}", raised.join(",")));
    }
    text
}

fn audit(agent: &Agent, done: Option<Done>) {
    let Some(done) = done else { return };
    crate::alerts::assistant::event(
        agent,
        crate::alerts::assistant::APPROVAL_ENDED,
        format!("AI assistant request ended · {}", agent.name()),
        Some(done.id),
    );
    if let Err(e) = crate::audit::record_assistant_request(
        &agent.db(),
        done.answered_by.as_deref(),
        &done.target,
        done.state,
    ) {
        tracing::warn!(id = done.id, "could not audit an assistant request: {e:#}");
    }
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
