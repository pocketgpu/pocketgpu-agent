//! Hook lists in an assistant's JSON settings: `{"hooks": {"<Event>": [...]}}` in Claude
//! Code, Codex, Cursor and Gemini CLI alike. Claude Code, Codex and Gemini CLI put each hook
//! in a group (`{"hooks": [hook]}`), Cursor lists hooks directly.

use anyhow::Result;
use serde_json::{Value, json};

use super::jsonedit;
pub use crate::cli::hook::Mode;

/// Whether a hook runs week 1's `pgpu hook <id>` (alerts), ours or written by hand.
#[cfg(test)]
pub fn is_ours(hook: &Value, id: &str) -> bool {
    is_ours_as(hook, id, Mode::Plain)
}

/// Whether a hook runs `pgpu hook <id>` in this mode: plain, `--ask` or `--check` (F32).
pub fn is_ours_as(hook: &Value, id: &str, mode: Mode) -> bool {
    let mut words: Vec<&str> = hook
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .split_whitespace()
        .collect();
    for arg in hook
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        words.push(arg.as_str().unwrap_or(""));
    }
    let line = words.join(" ");
    if !(line.contains("pocketgpu") || line.contains("pgpu")) {
        return false;
    }
    let Some(at) = words.windows(2).position(|w| w == ["hook", id]) else {
        return false;
    };
    let rest = &words[at + 2..];
    let has = |flag: &str| rest.contains(&flag);
    match mode {
        Mode::Plain => !has("--ask") && !has("--check"),
        Mode::Ask => has("--ask"),
        Mode::Check => has("--check"),
    }
}

/// The events in `events` that have no hook of ours yet.
pub fn missing<'a>(text: &str, events: &[&'a str], id: &str) -> Result<Vec<&'a str>> {
    missing_as(text, events, id, Mode::Plain)
}

/// The events in `events` that have no hook of ours in this mode yet.
pub fn missing_as<'a>(
    text: &str,
    events: &[&'a str],
    id: &str,
    mode: Mode,
) -> Result<Vec<&'a str>> {
    let mut out = Vec::new();
    for &event in events {
        let list = jsonedit::get(text, &["hooks", event])?.unwrap_or(Value::Null);
        let has = list.as_array().into_iter().flatten().any(|item| {
            is_ours_as(item, id, mode)
                || item
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hs| hs.iter().any(|h| is_ours_as(h, id, mode)))
        });
        if !has {
            out.push(event);
        }
    }
    Ok(out)
}

/// What adding hooks gives: the new text, the lists written to, and the objects and lists
/// that had to be made (to take back with them).
pub struct Added {
    pub text: String,
    pub lists: Vec<Vec<String>>,
    pub made: Vec<Vec<String>>,
}

/// Appends `entry(event)` to each event's list.
pub fn add(text: &str, events: &[&str], entry: impl Fn(&str) -> Value) -> Result<Added> {
    let mut text = text.to_string();
    let mut lists = Vec::new();
    let mut made = Vec::new();
    if jsonedit::get(&text, &["hooks"])?.is_none() {
        made.push(vec!["hooks".to_string()]);
    }
    for &event in events {
        let list = ["hooks", event];
        if jsonedit::get(&text, &list)?.is_none() {
            made.push(list.map(str::to_string).to_vec());
        }
        text = jsonedit::push(&text, &list, &entry(event))?;
        lists.push(list.map(str::to_string).to_vec());
    }
    Ok(Added { text, lists, made })
}

/// A Claude Code or Codex group holding one hook, with a matcher when it has one.
pub fn group(hook: &Value, matcher: Option<&str>) -> Value {
    match matcher {
        Some(m) => json!({ "matcher": m, "hooks": [hook] }),
        None => json!({ "hooks": [hook] }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ours_by_command_or_by_args() {
        assert!(is_ours(&json!({ "command": "pgpu hook claude" }), "claude"));
        assert!(is_ours(
            &json!({ "command": "C:\\PocketGPU\\bin\\pocketgpu.exe", "args": ["hook", "claude"] }),
            "claude"
        ));
        assert!(!is_ours(&json!({ "command": "pgpu hook codex" }), "claude"));
        assert!(!is_ours(
            &json!({ "command": "notify-send hook claude" }),
            "claude"
        ));
    }

    #[test]
    fn each_mode_is_its_own_hook() {
        let plain = json!({ "command": "'/my bin/pocketgpu' hook claude" });
        let ask = json!({ "command": "'/my bin/pocketgpu' hook claude --ask", "timeout": 330 });
        let check = json!({ "command": "C:\\PocketGPU\\pocketgpu.exe", "args": ["hook", "claude", "--check"] });
        assert!(is_ours(&plain, "claude"));
        assert!(!is_ours(&ask, "claude") && !is_ours(&check, "claude"));
        assert!(is_ours_as(&ask, "claude", Mode::Ask));
        assert!(!is_ours_as(&plain, "claude", Mode::Ask));
        assert!(!is_ours_as(&check, "claude", Mode::Ask));
        assert!(is_ours_as(&check, "claude", Mode::Check));
        assert!(!is_ours_as(&ask, "claude", Mode::Check));
        assert!(!is_ours_as(&ask, "claudex", Mode::Ask));
        assert!(!is_ours_as(&ask, "codex", Mode::Ask));
        let text = r#"{"hooks": {"PermissionRequest": [{"hooks": [{"command": "pgpu hook claude --ask"}]}]}}"#;
        let events = ["PermissionRequest"];
        assert!(
            missing_as(text, &events, "claude", Mode::Ask)
                .unwrap()
                .is_empty()
        );
        assert_eq!(missing(text, &events, "claude").unwrap(), events);
    }

    #[test]
    fn missing_sees_groups_and_bare_hooks() {
        let text = r#"{"hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "pgpu hook claude"}]}],
            "stop": [{"command": "pgpu hook claude"}],
            "Notification": [{"hooks": [{"command": "say hi"}]}]
        }}"#;
        let events = ["UserPromptSubmit", "Notification", "Stop", "stop"];
        assert_eq!(
            missing(text, &events, "claude").unwrap(),
            ["UserPromptSubmit", "Notification"]
        );
    }

    #[test]
    fn add_notes_what_it_made() {
        let text = "{\n  \"hooks\": {\n    \"Stop\": []\n  }\n}\n";
        let added = add(text, &["Stop", "UserPromptSubmit"], |e| json!({ "e": e })).unwrap();
        assert_eq!(added.made, [vec!["hooks", "UserPromptSubmit"]]);
        assert_eq!(added.lists.len(), 2);
        let v = jsonedit::read(&added.text).unwrap();
        assert_eq!(v["hooks"]["Stop"], json!([{ "e": "Stop" }]));
        assert_eq!(
            v["hooks"]["UserPromptSubmit"],
            json!([{ "e": "UserPromptSubmit" }])
        );
    }
}
