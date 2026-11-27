//! Why "Ask me first" can't work for an assistant here (F32, research §3), as `pgpu
//! assistants connect` recorded it in its manifest: Claude Code older than 2.1.295 lets a
//! `PreToolUse` call go ahead when its hook fails, so for it the switch would be a promise
//! that isn't kept. The agent refuses to arm "Ask me first" while any assistant is blocked,
//! except one whose command line isn't on PATH (only its version is unknown, say Claude
//! Code in an editor): that one keeps its own warning and doesn't hold back the others.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::store::config::AssistantConfig;

/// The oldest Claude Code whose `PreToolUse` hooks can block on failure (`onFailure`).
pub const CLAUDE_MIN: (u32, u32, u32) = (2, 1, 295);
pub const CLAUDE_MIN_TEXT: &str = "2.1.295";

/// How a block starts when the assistant's command line isn't on PATH.
pub const NOT_ON_PATH: &str = "Claude Code's command line isn't on PATH here";

/// The manifest `pgpu assistants connect` keeps in the agent's data folder.
pub fn manifest_path(data_dir: &Path) -> PathBuf {
    data_dir.join("assistants").join("changes.json")
}

/// Assistant id → why, from the manifest. A missing or unreadable manifest blocks nothing.
pub fn read(data_dir: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(manifest_path(data_dir)) else {
        return BTreeMap::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return BTreeMap::new();
    };
    v.get("check_blocked")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, why)| Some((k.clone(), why.as_str()?.to_string())))
        .collect()
}

/// Refuses settings that arm more of "Ask me first" (a kind or own word that wasn't on
/// before) while an assistant can't keep it. Turning things off is always allowed.
pub fn refuse_arming(
    blocked: &BTreeMap<String, String>,
    old: &AssistantConfig,
    next: &AssistantConfig,
) -> Result<(), String> {
    let more = next.ask_first.iter().any(|k| !old.ask_first.contains(k))
        || next.own_words.iter().any(|w| !old.own_words.contains(w));
    if !more {
        return Ok(());
    }
    match blocked.values().find(|why| !why.starts_with(NOT_ON_PATH)) {
        Some(why) => Err(format!("Ask me first can't be turned on: {why}")),
        None => Ok(()),
    }
}

/// "2.1.332 (Claude Code)" → (2, 1, 332).
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let word = text.split_whitespace().next()?;
    let mut parts = word.trim_start_matches('v').split('.');
    let mut num = || -> Option<u32> {
        let p = parts.next()?;
        let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    Some((num()?, num()?, num()?))
}

/// Why "Ask me first" can't work for Claude Code with no `claude` on PATH.
pub fn claude_not_on_path() -> String {
    format!(
        "{NOT_ON_PATH}, so its version couldn't be checked. Ask me first needs Claude Code \
         {CLAUDE_MIN_TEXT} or newer; put claude on PATH, then run pgpu assistants connect again."
    )
}

/// Why "Ask me first" can't work with this `claude --version` output, or `None` when it can.
pub fn claude_blocked(version_output: Option<&str>) -> Option<String> {
    let Some(text) = version_output else {
        return Some(format!(
            "Claude Code's version couldn't be read here. It needs Claude Code \
             {CLAUDE_MIN_TEXT} or newer; then run pgpu assistants connect again."
        ));
    };
    match parse_version(text) {
        Some(v) if v >= CLAUDE_MIN => None,
        Some((a, b, c)) => Some(format!(
            "Claude Code {a}.{b}.{c} is too old: it needs {CLAUDE_MIN_TEXT} or newer. \
             Update it, then run pgpu assistants connect again."
        )),
        None => claude_blocked(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("2.1.332 (Claude Code)"), Some((2, 1, 332)));
        assert_eq!(parse_version("v2.10.0"), Some((2, 10, 0)));
        assert_eq!(parse_version("2.1.295-beta.1"), Some((2, 1, 295)));
        assert_eq!(parse_version("Claude Code"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(claude_blocked(Some("2.1.295 (Claude Code)")), None);
        assert_eq!(claude_blocked(Some("3.0.0")), None);
        let old = claude_blocked(Some("2.1.200 (Claude Code)")).unwrap();
        assert!(old.contains("2.1.200 is too old") && old.contains("2.1.295"));
        assert!(claude_blocked(None).unwrap().contains("couldn't be read"));
        assert!(claude_blocked(Some("garbage")).is_some());
    }

    #[test]
    fn only_more_ask_me_first_is_refused() {
        let blocked = BTreeMap::from([("claude".to_string(), "too old".to_string())]);
        let off = AssistantConfig::default();
        let delete = AssistantConfig {
            ask_first: vec!["delete".into()],
            ..Default::default()
        };
        let words = AssistantConfig {
            own_words: vec!["terraform apply".into()],
            ..Default::default()
        };
        let phone = AssistantConfig {
            ask_on_phone: true,
            ..Default::default()
        };
        assert!(
            refuse_arming(&blocked, &off, &delete)
                .unwrap_err()
                .contains("too old")
        );
        assert!(refuse_arming(&blocked, &off, &words).is_err());
        assert!(refuse_arming(&blocked, &delete, &off).is_ok());
        assert!(refuse_arming(&blocked, &delete, &delete).is_ok());
        assert!(refuse_arming(&blocked, &off, &phone).is_ok());
        assert!(refuse_arming(&BTreeMap::new(), &off, &delete).is_ok());
        // Claude Code without its command line doesn't hold back Cursor's hooks.
        let unknown = BTreeMap::from([("claude".to_string(), claude_not_on_path())]);
        assert!(refuse_arming(&unknown, &off, &delete).is_ok());
        let both = BTreeMap::from([
            ("claude".to_string(), "too old".to_string()),
            ("other".to_string(), claude_not_on_path()),
        ]);
        assert!(refuse_arming(&both, &off, &delete).is_err());
    }

    #[test]
    fn read_from_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_empty());
        let path = manifest_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version":1,"changes":[],"check_blocked":{"claude":"old"}}"#,
        )
        .unwrap();
        assert_eq!(
            read(dir.path()).get("claude").map(String::as_str),
            Some("old")
        );
        std::fs::write(&path, "nope").unwrap();
        assert!(read(dir.path()).is_empty());
    }
}
