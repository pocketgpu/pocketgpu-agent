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
    // Tools, skill and alerts for each, with the shared skill written once, Claude Code's
    // answers from the phone and "Ask me first" for Claude Code and Cursor.
    assert_eq!(report.changes.len(), 11, "{text}");
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
        json!({ "command": exe, "args": ["mcp", "--assistant", "cursor"] })
    );
    let calls = env.fake.calls.borrow().join("\n");
    assert!(calls.contains(&format!(
        "claude mcp add --scope user pocketgpu -- {exe} mcp --assistant claude"
    )));
    assert!(calls.contains(&format!(
        "codex mcp add pocketgpu -- {exe} mcp --assistant codex"
    )));
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
    // Gemini CLI isn't in this home (see tests_gemini.rs).
    let here: Vec<_> = all.iter().filter(|s| s.found).collect();
    assert_eq!(here.len(), 3, "{all:?}");
    assert!(here.iter().all(|s| s.mcp && s.skill && s.hooks), "{all:?}");
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
    // Only `claude plugin list` and `--version`: nothing that changes anything ran.
    assert!(
        env.fake
            .calls
            .borrow()
            .iter()
            .all(|c| c.contains("plugin list") || c.ends_with("--version"))
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
    // Only the answers hooks, which do nothing until their switches are on.
    let cursor = env.json(&env.places.cursor_dir.join("hooks.json"));
    let hooks: Vec<&String> = cursor["hooks"].as_object().unwrap().keys().collect();
    assert_eq!(hooks, ["beforeMCPExecution", "beforeShellExecution"]);
    let claude = env.json(&env.places.claude_dir.join("settings.json"));
    let hooks: Vec<&String> = claude["hooks"].as_object().unwrap().keys().collect();
    assert_eq!(hooks, ["PermissionRequest", "PreToolUse", "Stop"]);
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
    // Only the `--check` hook, with the full path: the plugin can't carry a blocking hook
    // with a bare `pgpu` that may not start.
    let claude: Vec<Part> = report
        .changes
        .iter()
        .filter(|c| c.assistant == Assistant::Claude)
        .map(|c| c.part)
        .collect();
    assert_eq!(claude, [Part::Approvals], "{text}");
    let settings = env.json(&env.places.claude_dir.join("settings.json"));
    let check = &settings["hooks"]["PreToolUse"][0];
    assert_eq!(check["matcher"], super::claude_approvals::CHECK_MATCHER);
    let line = check["hooks"][0]["command"].as_str().unwrap_or_default();
    let full = check["hooks"][0]["command"] == json!(env.exe)
        || line.contains(&*env.exe.to_string_lossy());
    assert!(full && line != "pgpu hook claude --check", "{check}");
    assert!(settings["hooks"].get("PermissionRequest").is_none());
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

#[test]
fn claude_code_gets_answers_from_the_phone_in_their_own_step() {
    let env = env(&["claude", "codex"]);
    let before = snapshot(&env.places.home);
    let (report, text, _) = env.connect(YES, true);
    let report = report.unwrap();
    let approvals: Vec<_> = report
        .changes
        .iter()
        .filter(|c| c.part == Part::Approvals && c.assistant == Assistant::Claude)
        .collect();
    assert_eq!(approvals.len(), 2, "{text}");
    assert!(text.contains("answers from your phone"), "{text}");
    let claude = env.json(&env.places.claude_dir.join("settings.json"));
    let groups = claude["hooks"]["PermissionRequest"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert!(groups[0].get("matcher").is_none());
    let hook = &groups[0]["hooks"][0];
    assert!(
        hooks::is_ours_as(hook, "claude", hooks::Mode::Ask),
        "{hook}"
    );
    assert!(!hooks::is_ours(hook, "claude"));
    assert_eq!(hook["timeout"], 330);
    assert!(hook.get("async").is_none() && hook.get("onFailure").is_none());
    // The week-1 hooks gained PostToolUseFailure, as plain alert hooks.
    let failed = &claude["hooks"]["PostToolUseFailure"][0]["hooks"][0];
    assert!(hooks::is_ours(failed, "claude") && failed["async"] == true);
    // The manifest records it on its own, and status sees it.
    let manifest = Manifest::load(&env.places).unwrap();
    assert_eq!(
        manifest
            .changes
            .iter()
            .filter(|r| r.part == Part::Approvals && r.assistant == Assistant::Claude)
            .count(),
        2
    );
    let all = super::statuses(&env.ctx(&manifest)).unwrap();
    let claude_status = all
        .iter()
        .find(|s| s.assistant == Assistant::Claude)
        .unwrap();
    assert!(claude_status.ask && claude_status.check);
    assert!(
        all.iter()
            .filter(|s| s.assistant != Assistant::Claude)
            .all(|s| !s.ask)
    );
    assert!(super::list_words(&all).contains("answers ✓"));
    // Disconnect takes back exactly it, with everything else.
    env.disconnect();
    assert_eq!(snapshot(&env.places.home), before);
}

#[test]
fn a_week_one_connection_gets_only_the_missing_approval_step() {
    let env = env(&["claude", "codex"]);
    env.connect(YES, true).0.unwrap();
    // As week 1 left it: no PermissionRequest hook, and no record of one.
    let path = env.places.claude_dir.join("settings.json");
    let mut v = env.json(&path);
    v["hooks"]
        .as_object_mut()
        .unwrap()
        .remove("PermissionRequest");
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let mut manifest = Manifest::load(&env.places).unwrap();
    manifest.changes.retain(|r| r.part != Part::Approvals);
    manifest.save(&env.places).unwrap();
    let all = super::statuses(&env.ctx(&manifest)).unwrap();
    assert!(super::list_words(&all).contains("Connect them"));
    let (report, text, asked) = env.connect(YES, true);
    let parts: Vec<Part> = report.unwrap().changes.iter().map(|c| c.part).collect();
    assert_eq!(parts, [Part::Approvals], "{text}");
    assert_eq!(asked, 0);
    let back = env.disconnect();
    assert!(back.changes.iter().all(|c| c.ok), "{:?}", back.changes);
    let v = env.json(&path);
    assert!(v["hooks"].get("PermissionRequest").is_none(), "{v}");
}
