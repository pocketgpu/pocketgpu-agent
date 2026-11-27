//! Gemini CLI (F32): connect and disconnect against a made-up home folder, with a fake
//! `gemini` on PATH that fails if connect ever runs it.

use serde_json::json;

use super::gemini::{self, START};
use super::manifest::Manifest;
use super::plan::{Assistant, Part};
use super::test_env::{Env, YES, env, snapshot};
use super::{hooks, jsonedit, views};

const SETTINGS: &str = "{\n  // Gemini allows comments.\n  \"theme\": \"GitHub\",\n  \"mcpServers\": {\n    \"github\": { \"command\": \"gh-mcp\", \"env\": { \"TOKEN\": \"t\" } }\n  },\n  \"hooks\": {\n    \"AfterAgent\": [\n      { \"hooks\": [{ \"type\": \"command\", \"command\": \"say done\" }] }\n    ]\n  }\n}\n";
const NOTES: &str = "# My rules\n\nAlways use uv for Python.\n";

/// A home with only Gemini CLI's folder, holding settings and notes of the user's own.
fn gemini_home(have: &[&'static str]) -> Env {
    let env = env(have);
    for dir in [
        &env.places.claude_dir,
        &env.places.codex_home,
        &env.places.cursor_dir,
    ] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::remove_file(&env.places.claude_json).unwrap();
    std::fs::create_dir_all(&env.places.gemini_dir).unwrap();
    std::fs::write(gemini::settings(&env.places), SETTINGS).unwrap();
    std::fs::write(gemini::notes_file(&env.places), NOTES).unwrap();
    env
}

fn status(env: &Env) -> super::plan::Status {
    let manifest = Manifest::load(&env.places).unwrap();
    super::statuses(&env.ctx(&manifest))
        .unwrap()
        .into_iter()
        .find(|s| s.assistant == Assistant::Gemini)
        .unwrap()
}

#[test]
fn a_gemini_on_path_alone_gets_tools_notes_and_alerts() {
    let env = env(&["gemini"]);
    for dir in [
        &env.places.claude_dir,
        &env.places.codex_home,
        &env.places.cursor_dir,
    ] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::remove_file(&env.places.claude_json).unwrap();
    let before = snapshot(&env.places.home);
    let (report, text, _) = env.connect(YES, true);
    let report = report.unwrap();
    assert_eq!(report.found, [Assistant::Gemini], "{text}");
    assert!(report.changes.iter().all(|c| c.ok), "{text}");
    let parts: Vec<Part> = report.changes.iter().map(|c| c.part).collect();
    assert_eq!(parts, [Part::Mcp, Part::Skill, Part::Hooks], "{text}");
    // The fake fails on any command: connect writes the files itself.
    assert!(env.fake.calls.borrow().is_empty());

    let exe = env.exe.to_string_lossy().into_owned();
    let v = env.json(&gemini::settings(&env.places));
    assert_eq!(
        v["mcpServers"]["pocketgpu"],
        json!({ "command": exe, "args": ["mcp", "--assistant", "gemini"] })
    );
    for event in ["BeforeAgent", "Notification", "AfterTool", "AfterAgent"] {
        let hook = &v["hooks"][event][0]["hooks"][0];
        assert!(hooks::is_ours(hook, "gemini"), "{event}: {hook}");
        assert_eq!(hook["type"], "command");
        assert_eq!(hook["timeout"], 5000);
    }
    let notes = env.read(&gemini::notes_file(&env.places));
    assert_eq!(notes, gemini::block());
    assert!(notes.contains("pgpu send") && notes.contains("notify_phone"));
    assert!(!notes.contains("description:"), "no front matter");

    let s = status(&env);
    assert!(s.found && s.mcp && s.skill && s.hooks);
    assert!(!s.ask && !s.check, "alerts only");

    let back = env.disconnect();
    assert!(back.changes.iter().all(|c| c.ok), "{:?}", back.changes);
    assert_eq!(snapshot(&env.places.home), before);
    assert!(!env.places.manifest().exists());
}

#[test]
fn the_users_own_gemini_settings_and_notes_stay() {
    let env = gemini_home(&[]);
    let before = snapshot(&env.places.home);
    let (report, text, _) = env.connect(YES, true);
    assert!(report.unwrap().changes.iter().all(|c| c.ok), "{text}");
    let settings = env.read(&gemini::settings(&env.places));
    assert!(settings.starts_with("{\n  // Gemini allows comments.\n  \"theme\""));
    let v = jsonedit::read(&settings).unwrap();
    assert_eq!(v["mcpServers"]["github"]["env"]["TOKEN"], "t");
    assert_eq!(
        v["hooks"]["AfterAgent"][0]["hooks"][0]["command"],
        "say done"
    );
    assert!(hooks::is_ours(
        &v["hooks"]["AfterAgent"][1]["hooks"][0],
        "gemini"
    ));
    // The notes: theirs first, then ours after a blank line.
    let notes = env.read(&gemini::notes_file(&env.places));
    assert_eq!(notes, format!("{NOTES}\n{}", gemini::block()));

    // Connecting again changes nothing.
    let after = snapshot(&env.places.home);
    let (report, text, _) = env.connect(YES, true);
    assert!(report.unwrap().changes.is_empty(), "{text}");
    assert_eq!(snapshot(&env.places.home), after);

    let back = env.disconnect();
    assert!(back.changes.iter().all(|c| c.ok), "{:?}", back.changes);
    assert_eq!(snapshot(&env.places.home), before);
}

#[test]
fn disconnect_after_gemini_and_the_user_changed_the_files() {
    let env = gemini_home(&["gemini"]);
    env.connect(YES, true).0.unwrap();
    // Gemini saves a setting its own way; the user adds a line to their notes.
    let path = gemini::settings(&env.places);
    let mut v = env.json(&path);
    v["ui"] = json!({ "hideTips": true });
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let notes_path = gemini::notes_file(&env.places);
    let notes = env.read(&notes_path);
    std::fs::write(&notes_path, format!("{notes}\nPrefer small commits.\n")).unwrap();

    let back = env.disconnect();
    assert!(back.changes.iter().all(|c| c.ok), "{:?}", back.changes);
    let v = env.json(&path);
    assert_eq!(v["ui"]["hideTips"], true);
    assert_eq!(
        v["mcpServers"],
        json!({ "github": { "command": "gh-mcp", "env": { "TOKEN": "t" } } })
    );
    assert_eq!(
        v["hooks"],
        json!({ "AfterAgent": [{ "hooks": [{ "type": "command", "command": "say done" }] }] })
    );
    let notes = env.read(&notes_path);
    assert!(!notes.contains(START), "{notes}");
    assert!(notes.starts_with(NOTES) && notes.contains("Prefer small commits."));
}

#[test]
fn alerts_wait_for_a_yes_and_gemini_shows_alerts_only() {
    let env = gemini_home(&[]);
    let (report, text, asked) = env.connect(super::flow::Opts { yes: false, ..YES }, false);
    let report = report.unwrap();
    assert_eq!(asked, 1);
    assert!(!report.alerts);
    let parts: Vec<Part> = report.changes.iter().map(|c| c.part).collect();
    assert_eq!(parts, [Part::Mcp, Part::Skill], "{text}");
    assert!(!status(&env).hooks);

    // Saying yes later adds only the alerts, from the file as the tools left it.
    let (report, text, _) = env.connect(YES, true);
    let parts: Vec<Part> = report.unwrap().changes.iter().map(|c| c.part).collect();
    assert_eq!(parts, [Part::Hooks], "{text}");

    let manifest = Manifest::load(&env.places).unwrap();
    let all = super::statuses(&env.ctx(&manifest)).unwrap();
    let words = super::list_words(&all);
    assert!(
        words.contains("Gemini CLI   tools ✓  skill ✓  alerts ✓  (alerts only, no approvals)"),
        "{words}"
    );
    let view = views::recorded(&env.places.data_dir)
        .into_iter()
        .find(|v| v.id == "gemini")
        .unwrap();
    assert!(view.tools && view.alerts && !view.ask && !view.check);
    assert_eq!(view.name, "Gemini CLI");
}
