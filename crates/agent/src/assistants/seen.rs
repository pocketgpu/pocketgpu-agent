//! When each assistant was last heard from, and which approval hooks it called (F32, the AI
//! assistants screen). In memory only: a restarted agent knows nothing until the next hook.
//! It also lists assistants that connect didn't set up, such as Claude Code's plugin.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Names kept at most: hooks choose them, so a broken one can't grow this without end.
const MAX_NAMES: usize = 16;

/// What one assistant's hooks showed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Last {
    pub ms: i64,
    /// Its `--ask` hook (`AssistantAsk`) called at least once.
    pub ask: bool,
    /// Its `--check` hook (`AssistantCheck`) called at least once.
    pub check: bool,
}

/// Which hook a call came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    Alerts,
    Ask,
    Check,
}

#[derive(Debug, Default)]
pub struct Seen {
    names: Mutex<BTreeMap<String, Last>>,
}

impl Seen {
    /// Notes a call from `assistant` (already cleaned) at `now`.
    pub fn touch(&self, assistant: &str, hook: Hook, now: i64) {
        let id = canonical(assistant);
        let mut names = self.names.lock().expect("seen lock poisoned");
        if !names.contains_key(&id) && names.len() >= MAX_NAMES {
            let oldest = names
                .iter()
                .min_by_key(|(_, l)| l.ms)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                names.remove(&k);
            }
        }
        let last = names.entry(id).or_default();
        last.ms = last.ms.max(now);
        match hook {
            Hook::Alerts => {}
            Hook::Ask => last.ask = true,
            Hook::Check => last.check = true,
        }
    }

    /// Every assistant heard from since the agent started, by id.
    pub fn all(&self) -> BTreeMap<String, Last> {
        self.names.lock().expect("seen lock poisoned").clone()
    }
}

/// The id `connect` uses for an assistant's name as hooks give it.
pub fn canonical(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "claude-code" | "claudecode" | "claude_code" => "claude".into(),
        "gemini-cli" | "gemini_cli" => "gemini".into(),
        _ => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_time_and_the_hooks_seen() {
        let seen = Seen::default();
        seen.touch("Claude-Code", Hook::Alerts, 10);
        seen.touch("claude", Hook::Check, 5);
        seen.touch("claude", Hook::Ask, 20);
        let all = seen.all();
        assert_eq!(
            all["claude"],
            Last {
                ms: 20,
                ask: true,
                check: true
            }
        );
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn forgets_the_oldest_past_the_limit() {
        let seen = Seen::default();
        for i in 0..MAX_NAMES + 3 {
            seen.touch(&format!("a{i}"), Hook::Alerts, i as i64);
        }
        let all = seen.all();
        assert_eq!(all.len(), MAX_NAMES);
        assert!(!all.contains_key("a0"));
        assert!(all.contains_key(&format!("a{}", MAX_NAMES + 2)));
    }
}
