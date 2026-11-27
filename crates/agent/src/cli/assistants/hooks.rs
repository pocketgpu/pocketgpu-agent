//! Hook lists in an assistant's JSON settings: `{"hooks": {"<Event>": [...]}}` in Claude
//! Code, Codex and Cursor alike. Claude Code and Codex put each hook in a group
//! (`{"hooks": [hook]}`), Cursor lists hooks directly.

use anyhow::Result;
use serde_json::{Value, json};

use super::jsonedit;

/// Whether a hook runs `pgpu hook <id>`, ours or written by hand.
pub fn is_ours(hook: &Value, id: &str) -> bool {
    let mut line = hook
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    for arg in hook
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        line.push(' ');
        line.push_str(arg.as_str().unwrap_or(""));
    }
    line.contains(&format!("hook {id}")) && (line.contains("pocketgpu") || line.contains("pgpu"))
}

/// The events in `events` that have no hook of ours yet.
pub fn missing<'a>(text: &str, events: &[&'a str], id: &str) -> Result<Vec<&'a str>> {
    let mut out = Vec::new();
    for &event in events {
        let list = jsonedit::get(text, &["hooks", event])?.unwrap_or(Value::Null);
        let has = list.as_array().into_iter().flatten().any(|item| {
            is_ours(item, id)
                || item
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hs| hs.iter().any(|h| is_ours(h, id)))
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
