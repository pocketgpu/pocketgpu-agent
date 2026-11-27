//! `[assistants]` settings asked for at the computer (`pgpu assistants set`, F32): read, or
//! changed through [`super::hold::save_settings`], which checks, saves, rewrites the guard
//! file, audits as `local` and tells open Full control apps.

use pocketgpu_proto::assistants::AssistantSettings;
use pocketgpu_proto::local::Response;

use crate::daemon::state::Agent;
use crate::store::config::AssistantConfig;

/// `GetAssistantSettings` (`set: None`) and `SetAssistantSettings`.
pub fn local_request(agent: &Agent, set: Option<AssistantSettings>) -> Response {
    if let Some(next) = set {
        let saved = AssistantConfig::from_settings(&next)
            .and_then(|cfg| super::hold::save_settings(agent, cfg, "local"));
        if let Err(e) = saved {
            return Response::Error(e);
        }
    }
    let cfg = agent.config.read().expect("config lock poisoned");
    Response::AssistantSettings(cfg.assistants.to_settings())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fake::FakeGpus;
    use crate::daemon::state::Agent;
    use std::sync::Arc;

    #[test]
    fn set_saves_audits_and_arms_the_guard() {
        let agent = Agent::in_memory(Arc::new(FakeGpus::new(1))).unwrap();
        let Response::AssistantSettings(s) = local_request(&agent, None) else {
            panic!("no settings");
        };
        assert_eq!(s, AssistantSettings::default());
        let on = AssistantSettings {
            ask_on_phone: true,
            ..s
        };
        let Response::AssistantSettings(after) = local_request(&agent, Some(on.clone())) else {
            panic!("not saved");
        };
        assert_eq!(after, on);
        let saved = crate::store::config::Config::load(&agent.paths.config_file()).unwrap();
        assert!(saved.assistants.ask_on_phone);
        // "Ask on my phone" alone doesn't arm the guard: only "Ask me first" does.
        assert!(!super::super::guard::armed(&agent.paths));
        let rows = crate::audit::since(&agent.db(), 0).unwrap();
        assert!(rows.iter().any(|r| r.event == "assistant_settings"
            && r.device == "local"
            && r.result.contains("ask on phone on")));
        let bad = AssistantSettings {
            own_words: vec!["x".repeat(500)],
            ..on
        };
        assert!(matches!(
            local_request(&agent, Some(bad)),
            Response::Error(_)
        ));
    }

    #[test]
    fn ask_me_first_is_refused_while_claude_code_is_too_old() {
        use pocketgpu_proto::assistants::Kind;
        let agent = Agent::in_memory(Arc::new(FakeGpus::new(1))).unwrap();
        let manifest = crate::assistants::blocked::manifest_path(&agent.paths.data_dir);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(
            &manifest,
            r#"{"version":1,"changes":[],"check_blocked":{"claude":"Claude Code 2.1.200 is too old"}}"#,
        )
        .unwrap();
        let first = AssistantSettings {
            ask_first: vec![Kind::Delete],
            ..Default::default()
        };
        let Response::Error(why) = local_request(&agent, Some(first)) else {
            panic!("armed");
        };
        assert!(why.contains("Ask me first can't be turned on"), "{why}");
        assert!(why.contains("2.1.200 is too old"), "{why}");
        assert!(!super::super::guard::armed(&agent.paths));
        let words = AssistantSettings {
            own_words: vec!["terraform apply".into()],
            ..Default::default()
        };
        assert!(matches!(
            local_request(&agent, Some(words)),
            Response::Error(_)
        ));
        // "Ask on my phone" doesn't need the check.
        let phone = AssistantSettings {
            ask_on_phone: true,
            ..Default::default()
        };
        assert!(matches!(
            local_request(&agent, Some(phone)),
            Response::AssistantSettings(s) if s.ask_on_phone
        ));
    }
}
