//! `[assistants]` in `config.toml` (F32): what AI assistants ask on the phone. Changed with
//! `pgpu assistants set …` or the phone's `SetAssistantSettings`; checked on load and on set.

use std::collections::BTreeMap;

use pocketgpu_proto::Permission;
use pocketgpu_proto::assistants::{AssistantSettings, Kind, OWN_WORD_LEN, OWN_WORDS_MAX};
use serde::{Deserialize, Serialize};

/// Assistant levels kept at most.
pub const LEVELS_MAX: usize = 16;

/// An assistant's short name as levels keep it: 1 to 24 of `a-z`, `0-9` and `-`.
pub fn valid_assistant_id(id: &str) -> bool {
    (1..=24).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistantConfig {
    /// Questions an assistant would ask at the desk go to the phone too.
    pub ask_on_phone: bool,
    /// Kinds held for the phone in every mode: "delete", "git-push", "stop", "install".
    pub ask_first: Vec<String>,
    /// The owner's own words, held like `ask_first` (case-insensitive).
    pub own_words: Vec<String>,
    /// Per assistant id: "view", "control" or "shell". One missing means view.
    pub levels: BTreeMap<String, String>,
}

impl AssistantConfig {
    /// Every kind name known, at most `OWN_WORDS_MAX` words of 1 to `OWN_WORD_LEN`
    /// characters, and known levels. The error says what to fix.
    pub fn validate(&self) -> Result<(), String> {
        for name in &self.ask_first {
            if Kind::from_config_name(name).is_none() {
                return Err(format!(
                    "unknown kind {name:?} in ask_first; use delete, git-push, stop or install"
                ));
            }
        }
        if self.own_words.len() > OWN_WORDS_MAX {
            return Err(format!("at most {OWN_WORDS_MAX} own words"));
        }
        for word in &self.own_words {
            let len = word.trim().chars().count();
            if len == 0 || len > OWN_WORD_LEN {
                return Err(format!(
                    "each own word has 1 to {OWN_WORD_LEN} characters: {word:?}"
                ));
            }
        }
        if self.levels.len() > LEVELS_MAX {
            return Err(format!("at most {LEVELS_MAX} assistant levels"));
        }
        for (id, level) in &self.levels {
            if !valid_assistant_id(id) {
                return Err(format!(
                    "assistant {id:?} in levels: use its short name, such as claude"
                ));
            }
            level
                .parse::<Permission>()
                .map_err(|e| format!("level for {id}: {e}"))?;
        }
        Ok(())
    }

    /// The switchable kinds that are on, in their usual order.
    pub fn ask_first_kinds(&self) -> Vec<Kind> {
        Kind::ASK_FIRST
            .into_iter()
            .filter(|k| {
                self.ask_first
                    .iter()
                    .any(|n| Some(n.as_str()) == k.config_name())
            })
            .collect()
    }

    /// "Ask me first" holds something: the guard file says `armed`.
    pub fn armed(&self) -> bool {
        !self.ask_first.is_empty() || !self.own_words.is_empty()
    }

    /// The level for one assistant; missing (or unreadable) means View.
    pub fn level(&self, assistant: &str) -> Permission {
        self.levels
            .get(assistant)
            .and_then(|l| l.parse().ok())
            .unwrap_or(Permission::View)
    }

    pub fn to_settings(&self) -> AssistantSettings {
        AssistantSettings {
            ask_on_phone: self.ask_on_phone,
            ask_first: self.ask_first_kinds(),
            own_words: self.own_words.clone(),
            levels: self
                .levels
                .keys()
                .map(|id| (id.clone(), self.level(id)))
                .collect(),
        }
    }

    /// Settings from a phone: refused when a kind can't be switched or a word is too long.
    /// Words are trimmed, empty and repeated ones dropped.
    pub fn from_settings(s: &AssistantSettings) -> Result<Self, String> {
        let mut ask_first = Vec::new();
        for k in &s.ask_first {
            let Some(name) = k.config_name() else {
                return Err(format!("{k:?} can't be asked first"));
            };
            if !ask_first.iter().any(|n| n == name) {
                ask_first.push(name.to_string());
            }
        }
        let mut own_words: Vec<String> = Vec::new();
        for w in &s.own_words {
            let w = w.trim();
            if !w.is_empty() && !own_words.iter().any(|o| o.eq_ignore_ascii_case(w)) {
                own_words.push(w.to_string());
            }
        }
        let levels = s
            .levels
            .iter()
            .map(|(id, p)| (id.clone(), p.as_str().to_string()))
            .collect();
        let cfg = Self {
            ask_on_phone: s.ask_on_phone,
            ask_first,
            own_words,
            levels,
        };
        cfg.validate()?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::config::Config;

    #[test]
    fn off_in_an_older_config_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "name = \"rig-1\"\n").unwrap();
        let mut cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.assistants, AssistantConfig::default());
        assert!(!cfg.assistants.armed());

        cfg.assistants.ask_on_phone = true;
        cfg.assistants.ask_first = vec!["delete".into(), "git-push".into()];
        cfg.assistants.own_words = vec!["terraform apply".into()];
        cfg.assistants
            .levels
            .insert("claude".into(), "control".into());
        cfg.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back, cfg);
        assert!(back.assistants.armed());
        assert_eq!(
            back.assistants.ask_first_kinds(),
            vec![Kind::Delete, Kind::GitPush]
        );
        assert_eq!(back.assistants.level("claude"), Permission::Control);
        assert_eq!(back.assistants.level("codex"), Permission::View);
    }

    #[test]
    fn a_bad_hand_edit_stops_the_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for bad in [
            "[assistants]\nask_first = [\"delete\", \"format-disk\"]\n",
            "[assistants]\nown_words = [\"\"]\n",
            &format!("[assistants]\nown_words = [\"{}\"]\n", "x".repeat(65)),
            "[assistants.levels]\nclaude = \"root\"\n",
            "[assistants.levels]\n\"Claude Code\" = \"view\"\n",
        ] {
            std::fs::write(&path, bad).unwrap();
            let err = Config::load(&path).unwrap_err();
            assert!(
                format!("{err:#}").contains("[assistants]"),
                "{bad}: {err:#}"
            );
        }
        let many: Vec<String> = (0..21).map(|n| format!("\"w{n}\"")).collect();
        std::fs::write(
            &path,
            format!("[assistants]\nown_words = [{}]\n", many.join(", ")),
        )
        .unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn settings_from_a_phone() {
        let s = AssistantSettings {
            ask_on_phone: true,
            ask_first: vec![Kind::Install, Kind::Delete, Kind::Install],
            own_words: vec![
                " terraform apply ".into(),
                "TERRAFORM APPLY".into(),
                " ".into(),
            ],
            levels: vec![("claude".into(), Permission::Shell)],
        };
        let cfg = AssistantConfig::from_settings(&s).unwrap();
        assert_eq!(cfg.ask_first, vec!["install", "delete"]);
        assert_eq!(cfg.own_words, vec!["terraform apply"]);
        let back = cfg.to_settings();
        assert_eq!(back.ask_first, vec![Kind::Delete, Kind::Install]);
        assert_eq!(back.levels, vec![("claude".into(), Permission::Shell)]);

        for bad in [Kind::Other, Kind::OwnWords, Kind::Unknown] {
            let s = AssistantSettings {
                ask_first: vec![bad],
                ..Default::default()
            };
            assert!(AssistantConfig::from_settings(&s).is_err(), "{bad:?}");
        }
        let s = AssistantSettings {
            own_words: vec!["y".repeat(OWN_WORD_LEN + 1)],
            ..Default::default()
        };
        assert!(AssistantConfig::from_settings(&s).is_err());
    }
}
