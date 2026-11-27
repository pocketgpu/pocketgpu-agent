//! How many assistant pushes go out in an hour (F32). Each assistant has its own count, all of
//! them together have one more, and notes (`pgpu mcp`'s `notify_phone`) have their own for the
//! whole agent: every assistant session starts its own `pgpu mcp`, and any program on the
//! computer can call `pgpu hook` with a made-up name, so per-process or per-name counts alone
//! would let them flood the phone and the alert channels, and use up the push server's hourly
//! budget that job and GPU alerts need.

use std::collections::{HashMap, VecDeque};

/// Pushes per assistant per hour, the same as notes.
pub const PER_HOUR: usize = 10;
/// Notes per hour from every assistant session together.
pub const NOTES_PER_HOUR: usize = 10;
/// Assistant pushes and notes per hour in all, well under the push server's 60 an hour per
/// computer, so crash and temperature alerts still get through.
pub const ALL_PER_HOUR: usize = 20;
const HOUR_MS: i64 = 3_600_000;

#[derive(Debug, Default)]
pub struct Limits {
    /// When pushes went out, per [`key`].
    per_assistant: HashMap<&'static str, VecDeque<i64>>,
    notes: VecDeque<i64>,
    all: VecDeque<i64>,
}

impl Limits {
    /// Counts a push for this assistant if it and the total are under their limits.
    pub fn allow(&mut self, assistant: &str, now: i64) -> bool {
        self.forget_old(now);
        let times = self.per_assistant.entry(key(assistant)).or_default();
        if times.len() >= PER_HOUR || self.all.len() >= ALL_PER_HOUR {
            return false;
        }
        times.push_back(now);
        self.all.push_back(now);
        true
    }

    /// Counts a note if notes and the total are under their limits.
    pub fn allow_note(&mut self, now: i64) -> bool {
        self.forget_old(now);
        if self.notes.len() >= NOTES_PER_HOUR || self.all.len() >= ALL_PER_HOUR {
            return false;
        }
        self.notes.push_back(now);
        self.all.push_back(now);
        true
    }

    /// Drops what is more than an hour old.
    pub fn forget_old(&mut self, now: i64) {
        let recent = |t: &i64| now.saturating_sub(*t) < HOUR_MS;
        self.per_assistant.retain(|_, times| {
            times.retain(recent);
            !times.is_empty()
        });
        self.notes.retain(recent);
        self.all.retain(recent);
    }
}

/// The count an assistant's pushes go to: the ones `pgpu assistants connect` sets up have
/// their own; every other name shares one, so new names can't open new counts.
fn key(assistant: &str) -> &'static str {
    match assistant.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claudecode" => "claude",
        "codex" => "codex",
        "cursor" => "cursor",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn each_assistant_has_ten_an_hour() {
        let mut l = Limits::default();
        for i in 0..PER_HOUR as i64 {
            assert!(l.allow("claude", i * MIN), "push {i}");
        }
        assert!(!l.allow("claude", 30 * MIN));
        assert!(!l.allow("Claude-Code", 30 * MIN), "the same assistant");
        assert!(l.allow("codex", 30 * MIN));
        assert!(l.allow("claude", 60 * MIN), "an hour after the first");
    }

    #[test]
    fn made_up_names_share_one_count() {
        let mut l = Limits::default();
        let sent = (1..=30).filter(|i| l.allow(&format!("a{i}"), *i)).count();
        assert_eq!(sent, PER_HOUR);
        assert_eq!(l.per_assistant.len(), 1);
    }

    #[test]
    fn the_total_is_capped_across_assistants_and_notes() {
        let mut l = Limits::default();
        let mut sent = 0;
        for name in ["claude", "codex", "cursor", "gemini"] {
            sent += (0..PER_HOUR).filter(|_| l.allow(name, MIN)).count();
        }
        assert_eq!(sent, ALL_PER_HOUR);
        assert!(!l.allow_note(MIN), "notes count in the total too");
        assert!(l.allow_note(61 * MIN));
    }

    #[test]
    fn notes_are_ten_an_hour_for_the_whole_agent() {
        let mut l = Limits::default();
        for i in 0..NOTES_PER_HOUR as i64 {
            assert!(l.allow_note(i), "note {i}");
        }
        assert!(!l.allow_note(30 * MIN));
        // Hook pushes still have room in the total.
        assert!(l.allow("claude", 30 * MIN));
        assert!(l.allow_note(60 * MIN));
    }
}
