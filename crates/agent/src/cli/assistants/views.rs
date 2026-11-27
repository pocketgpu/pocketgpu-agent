//! What connect recorded per assistant, as the phone's AI assistants screen shows it (F32):
//! read from the manifest only, so the agent never runs an assistant's command line to
//! answer. An assistant set up by the Claude Code plugin alone has no records here.

use std::path::Path;

use pocketgpu_proto::Permission;
use pocketgpu_proto::assistants::AssistantView;

use super::hooks::{self, Mode};
use super::manifest::Manifest;
use super::plan::{Assistant, Change, FileUndo, Part};

/// One view per assistant connect changed something for, or found blocked. `last_seen_ms`
/// is `None` and `level` View; the agent fills them in.
pub fn recorded(data_dir: &Path) -> Vec<AssistantView> {
    let Ok(text) = std::fs::read_to_string(crate::assistants::blocked::manifest_path(data_dir))
    else {
        return Vec::new();
    };
    let Ok(m) = serde_json::from_str::<Manifest>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for a in Assistant::ALL {
        let mine: Vec<_> = m.changes.iter().filter(|r| r.assistant == a).collect();
        let blocked = m.check_blocked.get(a.id()).cloned();
        if mine.is_empty() && blocked.is_none() {
            continue;
        }
        let has = |part: Part| mine.iter().any(|r| r.part == part);
        let hook_as = |mode: Mode| {
            mine.iter().any(|r| match &r.change {
                Change::File {
                    undo: FileUndo::Hooks { hook, .. },
                    ..
                } => r.part == Part::Approvals && hooks::is_ours_as(hook, a.id(), mode),
                _ => false,
            })
        };
        out.push(AssistantView {
            id: a.id().to_string(),
            name: a.name().to_string(),
            last_seen_ms: None,
            alerts: has(Part::Hooks),
            tools: has(Part::Mcp),
            ask: hook_as(Mode::Ask),
            check: blocked.is_none() && hook_as(Mode::Check),
            check_blocked: blocked,
            level: Permission::View,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::test_env::{YES, env};
    use super::*;

    #[test]
    fn from_what_connect_recorded() {
        let mut env = env(&["claude"]);
        assert!(recorded(&env.places.data_dir).is_empty());
        env.fake.version = "2.1.100".into();
        env.connect(YES, true).0.unwrap();
        let views = recorded(&env.places.data_dir);
        let claude = views.iter().find(|v| v.id == "claude").unwrap();
        assert!(claude.tools && claude.alerts && claude.ask);
        assert!(!claude.check && claude.check_blocked.is_some());
        let cursor = views.iter().find(|v| v.id == "cursor").unwrap();
        assert!(cursor.check && !cursor.ask && cursor.check_blocked.is_none());
    }
}
