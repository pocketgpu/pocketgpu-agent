//! Connect and disconnect against a made-up home folder (see [`super::test_env`]).

use serde_json::json;

use super::flow::Opts;
use super::manifest::Manifest;
use super::plan::{Assistant, Part};
use super::test_env::{CODEX_CONFIG, CURSOR_MCP, SETTINGS, YES, env, snapshot};
use super::{hooks, jsonedit, skill};

#[test]
fn connect_then_disconnect_leaves_every_file_as_it_was() {
    let env = env(&["claude", "codex"]);
    let before = snapshot(&env.places.home);
    let (report, text, asked) = env.connect(YES, false);
    let report = report.unwrap();
    assert_eq!(asked, 0, "--yes doesn't ask");
    assert!(report.alerts);
    assert!(report.changes.iter().all(|c| c.ok), "{text}");
    // Tools, skill and alerts for each, with the shared skill written once.
    assert_eq!(report.changes.len(), 8, "{text}");
    assert!(
        text.contains("Found Claude Code, Codex and Cursor."),
        "{text}"
    );
    assert!(
        text.contains("Codex asks once to trust new hooks"),
        "{text}"
    );
    assert_ne!(snapshot(&env.places.home), before);

    let back = env.disconnect();
    assert!(back.changes.iter().all(|c| c.ok), "{:?}", back.changes);
    assert_eq!(snapshot(&env.places.home), before);
    assert!(!env.places.manifest().exists());
    assert!(!env.places.data_dir.join("assistants").exists());
}

#[test]
fn settings_name_this_program_by_its_full_path() {
    let env = env(&["claude", "codex"]);
    env.connect(YES, true).0.unwrap();
    let exe = env.exe.to_string_lossy().into_owned();
    let cursor = env.json(&env.places.cursor_dir.join("mcp.json"));
    assert_eq!(
        cursor["mcpServers"]["pocketgpu"],
        json!({ "command": exe, "args": ["mcp"] })
    );
    let calls = env.fake.calls.borrow().join("\n");
    assert!(calls.contains(&format!(
        "claude mcp add --scope user pocketgpu -- {exe} mcp"
    )));
    assert!(calls.contains(&format!("codex mcp add pocketgpu -- {exe} mcp")));
    let claude = env.json(&env.places.claude_dir.join("settings.json"));
    for event in ["UserPromptSubmit", "Notification", "Stop", "PostToolUse"] {
        let hook = claude["hooks"][event].as_array().unwrap().last().unwrap()["hooks"][0].clone();
        assert!(hooks::is_ours(&hook, "claude"), "{event}: {hook}");
        assert!(
            hook.to_string().contains(&exe.replace('\\', "\\\\")),
            "{hook}"
        );
        assert_eq!(hook["async"], true);
    }
    assert_eq!(
        claude["hooks"]["Notification"][0]["matcher"],
        "permission_prompt|idle_prompt"
    );
    let cursor_hooks = env.json(&env.places.cursor_dir.join("hooks.json"));
    assert_eq!(cursor_hooks["version"], 1);
    assert!(hooks::is_ours(&cursor_hooks["hooks"]["stop"][0], "cursor"));
    assert!(hooks::is_ours(
        &cursor_hooks["hooks"]["beforeSubmitPrompt"][0],
        "cursor"
    ));
    for skills in [&env.places.claude_dir, &env.places.agents_dir] {
        let path = skills.join("skills/pocketgpu/SKILL.md");
        assert_eq!(env.read(&path), skill::TEXT);
    }
}

#[test]
fn the_users_own_settings_stay() {
    let env = env(&["claude", "codex"]);
    env.connect(YES, true).0.unwrap();
    let settings = env.read(&env.places.claude_dir.join("settings.json"));
    // Their keys keep their order and their hook stays first.
    assert!(settings.starts_with("{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"Stop\": [\n"));
    let v = jsonedit::read(&settings).unwrap();
    assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
    assert!(
        v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("afplay")
    );
    assert_eq!(v["enabledPlugins"], json!({ "other@market": true }));
    // Codex: the notify program and the user's hook are left alone.
    let config = env.read(&env.places.codex_home.join("config.toml"));
    assert!(config.contains("notify = [\"say\", \"done\"]"));
    let codex = env.json(&env.places.codex_home.join("hooks.json"));
    assert_eq!(codex["hooks"]["Stop"][0]["hooks"][0]["command"], "mine");
    assert!(hooks::is_ours(
        &codex["hooks"]["Stop"][1]["hooks"][0],
        "codex"
    ));
    // Cursor: the other server and its env, unchanged and first.
    let mcp = env.read(&env.places.cursor_dir.join("mcp.json"));
    assert!(
        mcp.starts_with(&CURSOR_MCP[..CURSOR_MCP.len() - 8]),
        "{mcp}"
    );
    assert!(mcp.contains("\"TOKEN\": \"t\""));
}

#[test]
fn connecting_again_changes_nothing() {
    let env = env(&["claude", "codex"]);
    env.connect(YES, true).0.unwrap();
    let after = snapshot(&env.places.home);
    let manifest = env.read(&env.places.manifest());
    let (report, text, _) = env.connect(YES, true);
    let report = report.unwrap();
    assert!(report.changes.is_empty(), "{text}");
    assert!(text.contains("Everything is connected already."), "{text}");
    assert_eq!(snapshot(&env.places.home), after);
    assert_eq!(env.read(&env.places.manifest()), manifest);
    let ctx_manifest = Manifest::load(&env.places).unwrap();
    let all = super::statuses(&env.ctx(&ctx_manifest)).unwrap();
    assert!(
        all.iter().all(|s| s.found && s.mcp && s.skill && s.hooks),
        "{all:?}"
    );
}

#[test]
fn a_dry_run_only_shows() {
    let env = env(&["claude", "codex"]);
    let before = snapshot(&env.places.home);
    let opts = Opts {
        dry_run: true,
        yes: false,
        json: false,
    };
    let (report, text, asked) = env.connect(opts, true);
    assert!(report.unwrap().changes.is_empty());
    assert_eq!(asked, 0);
    assert!(text.contains("This adds:") && text.contains("And, if you say yes, alerts:"));
    assert!(
        text.contains("~/.cursor/mcp.json (a backup is kept)"),
        "{text}"
    );
    assert!(text.contains("Nothing was changed (--dry-run)."));
    assert_eq!(snapshot(&env.places.home), before);
    assert!(!env.places.data_dir.exists());
    // Only `claude plugin list`: nothing that changes anything ran.
    assert!(
        env.fake
            .calls
            .borrow()
            .iter()
            .all(|c| c.contains("plugin list"))
    );
}

#[test]
fn alerts_wait_for_a_yes() {
    let env = env(&["claude", "codex"]);
    let opts = Opts { yes: false, ..YES };
    let (report, text, asked) = env.connect(opts, false);
    let report = report.unwrap();
    assert_eq!(asked, 1);
    assert!(!report.alerts);
    assert!(report.changes.iter().all(|c| c.part != Part::Hooks));
    assert!(text.contains("No alerts for now"), "{text}");
    assert!(!env.places.cursor_dir.join("hooks.json").exists());
    assert_eq!(
        env.read(&env.places.claude_dir.join("settings.json")),
        SETTINGS
    );
    // Asked again next time, and a yes adds only the alerts.
    let (report, _, asked) = env.connect(opts, true);
    assert_eq!(asked, 1);
    let parts: Vec<Part> = report.unwrap().changes.iter().map(|c| c.part).collect();
    assert_eq!(parts, [Part::Hooks; 3]);
}

#[test]
fn the_claude_code_plugin_covers_claude_code() {
    let mut env = env(&["claude", "codex"]);
    env.fake.plugins = r#"[{"id": "pocketgpu@pocketgpu", "enabled": true}]"#.into();
    let (report, text, _) = env.connect(YES, true);
    let report = report.unwrap();
    assert!(
        report
            .changes
            .iter()
            .all(|c| c.assistant != Assistant::Claude),
        "{text}"
    );
    assert_eq!(
        env.read(&env.places.claude_dir.join("settings.json")),
        SETTINGS
    );
    assert!(!env.places.claude_dir.join("skills").exists());
    assert!(
        !env.fake
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("claude mcp"))
    );
}

#[test]
fn disconnect_after_the_assistant_rewrote_its_settings() {
    let env = env(&["claude", "codex"]);
    env.connect(YES, true).0.unwrap();
    // Claude Code saves its settings its own way, with a new key of the user's.
    let path = env.places.claude_dir.join("settings.json");
    let mut v = env.json(&path);
    v["theme"] = json!("dark");
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    // Cursor's hooks file gets a hook of the user's.
    let cursor = env.places.cursor_dir.join("hooks.json");
    let text = jsonedit::push(
        &env.read(&cursor),
        &["hooks", "stop"],
        &json!({ "command": "x" }),
    )
    .unwrap();
    std::fs::write(&cursor, text).unwrap();

    let back = env.disconnect();
    assert!(back.changes.iter().all(|c| c.ok), "{:?}", back.changes);
    let v = env.json(&path);
    assert_eq!(v["theme"], "dark");
    assert_eq!(v["hooks"], jsonedit::read(SETTINGS).unwrap()["hooks"]);
    assert_eq!(
        env.json(&cursor),
        json!({ "version": 1, "hooks": { "stop": [{ "command": "x" }] } })
    );
    assert!(!env.places.manifest().exists());
}

#[test]
fn codex_without_its_command_line_gets_a_table() {
    let env = env(&["claude"]);
    let before = snapshot(&env.places.home);
    env.connect(YES, true).0.unwrap();
    let config = env.read(&env.places.codex_home.join("config.toml"));
    assert!(config.starts_with(CODEX_CONFIG));
    let table: toml::Table = toml::from_str(&config).unwrap();
    assert_eq!(
        table["mcp_servers"]["pocketgpu"]["command"].as_str(),
        Some(env.exe.to_string_lossy().as_ref())
    );
    env.disconnect();
    assert_eq!(snapshot(&env.places.home), before);
}

#[test]
fn nothing_found_says_where_to_look() {
    let env = env(&[]);
    for dir in [
        &env.places.claude_dir,
        &env.places.codex_home,
        &env.places.cursor_dir,
    ] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    let (report, text, _) = env.connect(YES, true);
    assert!(report.unwrap().found.is_empty());
    assert!(text.contains("No AI assistants found") && text.contains(super::DOCS));
    let mut out = Vec::new();
    let manifest = Manifest::default();
    super::list(&env.ctx(&manifest), false, &mut out).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("not found"));
}

#[test]
fn an_older_skill_of_ours_is_brought_up_to_date() {
    let env = env(&["claude", "codex"]);
    env.connect(YES, true).0.unwrap();
    // Make the recorded skill look like an older version's.
    let path = env.places.claude_dir.join("skills/pocketgpu/SKILL.md");
    let old = "---\nname: pocketgpu\ndescription: old\n---\n";
    std::fs::write(&path, old).unwrap();
    let text = env.read(&env.places.manifest());
    let text = text.replace(&super::files::hash(skill::TEXT), &super::files::hash(old));
    std::fs::write(env.places.manifest(), text).unwrap();
    let (report, _, _) = env.connect(YES, true);
    let parts: Vec<Part> = report.unwrap().changes.iter().map(|c| c.part).collect();
    assert_eq!(parts, [Part::Skill]);
    assert_eq!(env.read(&path), skill::TEXT);
    // A skill of the user's own with the name is left alone.
    std::fs::write(&path, "mine").unwrap();
    assert!(env.connect(YES, true).0.unwrap().changes.is_empty());
}
