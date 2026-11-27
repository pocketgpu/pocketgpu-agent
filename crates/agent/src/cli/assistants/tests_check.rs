//! "Ask me first" hooks from connect (F32): Claude Code's `PreToolUse` `--check` entry only
//! for a new enough Claude Code, Cursor's `failClosed` entries, and why it is blocked
//! recorded for the agent.

use super::claude_approvals::{CHECK_EVENT, CHECK_MATCHER};
use super::hooks::{self, Mode};
use super::manifest::Manifest;
use super::plan::{Assistant, Part};
use super::test_env::{YES, env, snapshot};

#[test]
fn claude_code_gets_a_blocking_pre_tool_use_check() {
    let env = env(&["claude", "codex"]);
    let before = snapshot(&env.places.home);
    let (report, text, _) = env.connect(YES, true);
    assert!(report.unwrap().changes.iter().all(|c| c.ok), "{text}");
    let claude = env.json(&env.places.claude_dir.join("settings.json"));
    let groups = claude["hooks"][CHECK_EVENT].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["matcher"], CHECK_MATCHER);
    let hook = &groups[0]["hooks"][0];
    assert!(hooks::is_ours_as(hook, "claude", Mode::Check), "{hook}");
    assert!(!hooks::is_ours_as(hook, "claude", Mode::Ask));
    assert_eq!(hook["timeout"], 330);
    assert_eq!(hook["onFailure"], "block");
    assert!(hook.get("async").is_none());

    // Cursor: both `--check` hooks, failing closed.
    let cursor = env.json(&env.places.cursor_dir.join("hooks.json"));
    for event in ["beforeShellExecution", "beforeMCPExecution"] {
        let hook = &cursor["hooks"][event][0];
        assert!(hooks::is_ours_as(hook, "cursor", Mode::Check), "{hook}");
        assert_eq!(hook["timeout"], 330, "{event}");
        assert_eq!(hook["failClosed"], true, "{event}");
    }
    assert_eq!(cursor["version"], 1);

    let manifest = Manifest::load(&env.places).unwrap();
    assert!(manifest.check_blocked.is_empty());
    let all = super::statuses(&env.ctx(&manifest)).unwrap();
    for a in [Assistant::Claude, Assistant::Cursor] {
        let s = all.iter().find(|s| s.assistant == a).unwrap();
        assert!(s.check && s.check_blocked.is_none(), "{a:?}");
    }
    let words = super::list_words(&all);
    assert!(words.contains("ask first ✓"), "{words}");
    assert!(words.contains("Turn it off"), "{words}");

    // Connecting again changes nothing; disconnect takes it all back.
    let (report, _, _) = env.connect(YES, true);
    assert!(report.unwrap().changes.is_empty());
    env.disconnect();
    assert_eq!(snapshot(&env.places.home), before);
    assert!(!env.places.manifest().exists());
}

#[test]
fn an_old_claude_code_gets_no_check_and_says_why() {
    let mut env = env(&["claude"]);
    env.fake.version = "2.1.200 (Claude Code)\n".into();
    let (report, text, _) = env.connect(YES, true);
    let report = report.unwrap();
    assert!(
        text.contains("Ask me first is off for Claude Code"),
        "{text}"
    );
    assert!(text.contains("2.1.295"), "{text}");
    // "Ask on my phone" still works: its hook goes in.
    let claude_parts: Vec<_> = report
        .changes
        .iter()
        .filter(|c| c.assistant == Assistant::Claude && c.part == Part::Approvals)
        .collect();
    assert_eq!(claude_parts.len(), 1);
    let claude = env.json(&env.places.claude_dir.join("settings.json"));
    assert!(claude["hooks"].get(CHECK_EVENT).is_none(), "{claude}");
    // Recorded for the agent, which then refuses to arm "Ask me first".
    let blocked = crate::assistants::blocked::read(&env.places.data_dir);
    assert!(
        blocked["claude"].contains("2.1.200 is too old"),
        "{blocked:?}"
    );
    let manifest = Manifest::load(&env.places).unwrap();
    let all = super::statuses(&env.ctx(&manifest)).unwrap();
    let s = all
        .iter()
        .find(|s| s.assistant == Assistant::Claude)
        .unwrap();
    assert!(!s.check && s.check_blocked.is_some());
    let words = super::list_words(&all);
    assert!(
        words.contains("Ask me first is off: Claude Code 2.1.200"),
        "{words}"
    );

    // Updated: connect again adds the check and clears the block.
    env.fake.version = "2.1.332 (Claude Code)\n".into();
    let (report, text, _) = env.connect(YES, true);
    let parts: Vec<Part> = report.unwrap().changes.iter().map(|c| c.part).collect();
    assert_eq!(parts, [Part::Approvals], "{text}");
    assert!(crate::assistants::blocked::read(&env.places.data_dir).is_empty());
    let claude = env.json(&env.places.claude_dir.join("settings.json"));
    assert_eq!(claude["hooks"][CHECK_EVENT][0]["matcher"], CHECK_MATCHER);
}

#[test]
fn with_the_plugin_list_checks_the_version() {
    let mut env = env(&["claude"]);
    env.fake.plugins = r#"[{"id": "pocketgpu@pocketgpu", "enabled": true}]"#.into();
    env.fake.version = "2.0.9\n".into();
    let manifest = Manifest::load(&env.places).unwrap();
    let mut out = Vec::new();
    super::list(&env.ctx(&manifest), false, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Ask me first is off"), "{text}");
    assert!(crate::assistants::blocked::read(&env.places.data_dir).contains_key("claude"));
    // A new enough one clears it again.
    env.fake.version = "2.1.295\n".into();
    let manifest = Manifest::load(&env.places).unwrap();
    super::list(&env.ctx(&manifest), false, &mut Vec::new()).unwrap();
    assert!(crate::assistants::blocked::read(&env.places.data_dir).is_empty());
    assert!(!env.places.manifest().exists());
}
