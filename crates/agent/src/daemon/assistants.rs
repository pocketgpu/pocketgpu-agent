//! The phone's side of AI assistants (F32, contracts §3): `ListApprovals`, `AnswerApproval`,
//! `SetAssistantSettings` and `AssistantActivity` need Shell; `GetAssistants` needs View and
//! gives the settings to Shell only. Levels are checked by `control::handle` before this,
//! from the database each time; guests are refused here too, whatever their level, and
//! [`crate::assistants::hold::answer`] reads the device once more before it counts.

use std::collections::BTreeMap;

use pocketgpu_proto::Permission;
use pocketgpu_proto::assistants::{AssistantSettings, AssistantView};
use pocketgpu_proto::msg::control::Response;
use pocketgpu_proto::{Error, ErrorCode};

use super::auth::Peer;
use super::state::Agent;
use crate::assistants::approvals::TOO_LONG;
use crate::assistants::hold;
use crate::assistants::seen::Last;
use crate::store::config::AssistantConfig;
use crate::store::db::now_ms;

const GUESTS: &str = "guests can't see or answer AI assistants";

/// Open requests and those that ended in the last 10 minutes, newest first.
pub fn list(agent: &Agent, peer: &Peer) -> Response {
    if let Err(e) = owner(agent, peer) {
        return Response::Error(e);
    }
    Response::Approvals(agent.assistants.approvals.list(now_ms()))
}

/// Allow or Deny from a Full control phone. The first answer wins; a stale `id` or
/// `opened_ms` (a request from before a restart, say) is refused.
pub fn answer(agent: &Agent, peer: &Peer, id: u64, opened_ms: i64, allow: bool) -> Response {
    if let Err(e) = owner(agent, peer) {
        return Response::Error(e);
    }
    match hold::answer(agent, &peer.key.to_string(), id, opened_ms, allow) {
        Ok(view) => Response::ApprovalAnswered(Box::new(view)),
        Err(why) => Response::Error(Error::new(code(&why), why)),
    }
}

/// New settings from a Full control phone: checked, saved, audited with the phone's key,
/// the guard file rewritten and open apps told (`assistant_settings`), all in
/// [`hold::save_settings`]. More of "Ask me first" is refused while an assistant here can't
/// keep it. Replies with the assistants and the settings as saved.
pub fn set_settings(agent: &Agent, peer: &Peer, settings: AssistantSettings) -> Response {
    if let Err(e) = owner(agent, peer) {
        return Response::Error(e);
    }
    let saved = AssistantConfig::from_settings(&settings)
        .and_then(|cfg| hold::save_settings(agent, cfg, &peer.key.to_string()));
    if let Err(why) = saved {
        return Response::Error(Error::new(ErrorCode::Invalid, why));
    }
    let settings = agent
        .config
        .read()
        .expect("config lock poisoned")
        .assistants
        .to_settings();
    Response::Assistants {
        connected: connected(agent),
        settings: Some(settings),
    }
}

/// Who is connected, for every phone; the settings only for the owner's Full control
/// devices (View and Control phones, and guests, see the assistants alone).
pub fn get(agent: &Agent, peer: &Peer) -> Response {
    let shell = agent.permission_of(&peer.key) == Some(Permission::Shell);
    let settings = (shell && owner(agent, peer).is_ok()).then(|| {
        agent
            .config
            .read()
            .expect("config lock poisoned")
            .assistants
            .to_settings()
    });
    Response::Assistants {
        connected: connected(agent),
        settings,
    }
}

/// The `assistant_*` audit entries since `since_ms`, newest first, at most 200, with each
/// phone's name. Summaries only: the audit keeps no command whole.
pub fn activity(agent: &Agent, peer: &Peer, since_ms: i64) -> Response {
    if let Err(e) = owner(agent, peer) {
        return Response::Error(e);
    }
    match crate::audit::assistant_activity(&agent.db(), since_ms) {
        Ok(list) => Response::Activity(list),
        Err(e) => Response::Error(Error::new(ErrorCode::Internal, e.to_string())),
    }
}

/// The assistants connect set up here, then any other heard from since the agent started
/// (Claude Code's plugin, hooks added by hand), each with its level and when it was last
/// heard from.
pub fn connected(agent: &Agent) -> Vec<AssistantView> {
    let cfg = agent
        .config
        .read()
        .expect("config lock poisoned")
        .assistants
        .clone();
    let recorded = crate::cli::assistants::recorded(&agent.paths.data_dir);
    merge(recorded, agent.assistants.seen.all())
        .into_iter()
        .map(|v| AssistantView {
            level: cfg.level(&v.id),
            ..v
        })
        .collect()
}

/// `recorded` with `last_seen_ms` filled in, then the assistants only hooks told of: their
/// alerts work, and an approval hook counts once it has called.
fn merge(recorded: Vec<AssistantView>, seen: BTreeMap<String, Last>) -> Vec<AssistantView> {
    let mut out: Vec<AssistantView> = recorded
        .into_iter()
        .map(|v| AssistantView {
            last_seen_ms: seen.get(&v.id).map(|l| l.ms),
            ..v
        })
        .collect();
    for (id, last) in seen {
        if id == "unknown" || out.iter().any(|v| v.id == id) {
            continue;
        }
        out.push(AssistantView {
            name: crate::assistants::display_name(&id),
            id,
            last_seen_ms: Some(last.ms),
            alerts: true,
            tools: false,
            ask: last.ask,
            check: last.check,
            check_blocked: None,
            level: Permission::View,
        });
    }
    out
}

/// The owner's device, paired for good. A database error counts as a guest.
fn owner(agent: &Agent, peer: &Peer) -> Result<(), Error> {
    match crate::store::devices::is_guest(&agent.db(), &peer.key.to_string()) {
        Ok(false) => Ok(()),
        _ => Err(Error::new(ErrorCode::Forbidden, GUESTS)),
    }
}

/// How the app tells refusals apart: gone, already answered, too long, or not allowed.
fn code(why: &str) -> ErrorCode {
    if why == "no longer open" {
        ErrorCode::NotFound
    } else if why.starts_with("already ") {
        ErrorCode::Busy
    } else if why == TOO_LONG {
        ErrorCode::Invalid
    } else {
        ErrorCode::Forbidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str) -> AssistantView {
        AssistantView {
            id: id.into(),
            name: crate::assistants::display_name(id),
            last_seen_ms: None,
            alerts: true,
            tools: true,
            ask: true,
            check: true,
            check_blocked: None,
            level: Permission::View,
        }
    }

    #[test]
    fn heard_from_assistants_join_the_recorded_ones() {
        let mut seen = BTreeMap::new();
        let last = |ms, ask, check| Last { ms, ask, check };
        seen.insert("codex".to_string(), last(50, false, false));
        seen.insert("claude".to_string(), last(70, true, false));
        seen.insert("unknown".to_string(), last(80, true, true));
        let got = merge(vec![view("codex")], seen);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "codex");
        assert_eq!(got[0].last_seen_ms, Some(50));
        // Still what connect wrote, not what the hooks showed.
        assert!(got[0].ask && got[0].tools);
        assert_eq!(got[1].id, "claude");
        assert_eq!(got[1].name, "Claude Code");
        assert_eq!(got[1].last_seen_ms, Some(70));
        assert!(got[1].alerts && got[1].ask && !got[1].check && !got[1].tools);
        let none = merge(vec![view("cursor")], BTreeMap::new());
        assert_eq!(none[0].last_seen_ms, None);
    }

    #[test]
    fn refusals_have_codes_the_app_can_tell_apart() {
        assert_eq!(code("no longer open"), ErrorCode::NotFound);
        assert_eq!(code("already allowed on Pixel"), ErrorCode::Busy);
        assert_eq!(code("already answered at the computer"), ErrorCode::Busy);
        assert_eq!(code(TOO_LONG), ErrorCode::Invalid);
        assert_eq!(
            code("answering AI assistants needs Full control"),
            ErrorCode::Forbidden
        );
    }
}
