//! PocketGPU's own clipboard for machines with no screen (F41): text in the agent's memory,
//! never written to disk, forgotten after [`CLEARS_AFTER`] or, when asked, after its first
//! read. Callers pass the time, so tests can move the clock.

use std::time::{Duration, Instant};

use pocketgpu_proto::clip::CLEARS_AFTER_S;

pub const CLEARS_AFTER: Duration = Duration::from_secs(CLEARS_AFTER_S as u64);

#[derive(Debug, Default)]
pub struct Memory {
    held: Option<Held>,
}

#[derive(Debug)]
struct Held {
    text: String,
    at: Instant,
    once: bool,
}

impl Memory {
    /// Replaces what is there. `once`: forget it after the first read.
    pub fn put(&mut self, text: String, once: bool, now: Instant) {
        self.held = Some(Held {
            text,
            at: now,
            once,
        });
    }

    /// The text and how long ago it was put; a `once` text is gone after this.
    pub fn get(&mut self, now: Instant) -> Option<(String, Duration)> {
        self.expire(now);
        let held = self.held.as_ref()?;
        let age = now.saturating_duration_since(held.at);
        let text = if held.once {
            self.held.take()?.text
        } else {
            held.text.clone()
        };
        Some((text, age))
    }

    pub fn has_text(&mut self, now: Instant) -> bool {
        self.clears_in(now).is_some()
    }

    /// How long until the text is forgotten; `None` when there is none.
    pub fn clears_in(&mut self, now: Instant) -> Option<Duration> {
        self.expire(now);
        let held = self.held.as_ref()?;
        Some(CLEARS_AFTER.saturating_sub(now.saturating_duration_since(held.at)))
    }

    pub fn clear(&mut self) {
        self.held = None;
    }

    /// Forgets the text once it is [`CLEARS_AFTER`] old.
    pub fn expire(&mut self, now: Instant) {
        if self
            .held
            .as_ref()
            .is_some_and(|h| now.saturating_duration_since(h.at) >= CLEARS_AFTER)
        {
            self.held = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_text_for_ten_minutes() {
        let start = Instant::now();
        let mut m = Memory::default();
        assert!(m.get(start).is_none());
        m.put("hf_abc".into(), false, start);

        let later = start + Duration::from_secs(90);
        assert_eq!(
            m.get(later),
            Some(("hf_abc".into(), Duration::from_secs(90)))
        );
        // Reading doesn't use it up.
        assert!(m.has_text(later));
        assert_eq!(m.clears_in(later), Some(Duration::from_secs(510)));

        let almost = start + CLEARS_AFTER - Duration::from_millis(1);
        assert!(m.get(almost).is_some());
        assert!(m.get(start + CLEARS_AFTER).is_none());
        // Gone for good, not just hidden.
        assert!(m.get(start).is_none());
        assert_eq!(m.clears_in(start), None);
    }

    #[test]
    fn once_is_forgotten_after_the_first_read() {
        let start = Instant::now();
        let mut m = Memory::default();
        m.put("token".into(), true, start);
        assert!(m.has_text(start));
        assert_eq!(m.get(start).map(|(t, _)| t).as_deref(), Some("token"));
        assert!(m.get(start).is_none());
        assert!(!m.has_text(start));
    }

    #[test]
    fn a_new_put_replaces_the_old_and_restarts_the_clock() {
        let start = Instant::now();
        let mut m = Memory::default();
        m.put("old".into(), true, start);
        let later = start + Duration::from_secs(500);
        m.put("new".into(), false, later);
        assert_eq!(m.clears_in(later), Some(CLEARS_AFTER));
        let t = start + CLEARS_AFTER + Duration::from_secs(1);
        assert_eq!(m.get(t).map(|(t, _)| t).as_deref(), Some("new"));
        m.clear();
        assert!(m.get(t).is_none());
    }

    #[test]
    fn expire_drops_only_old_text() {
        let start = Instant::now();
        let mut m = Memory::default();
        m.put("x".into(), false, start);
        m.expire(start + Duration::from_secs(1));
        assert!(m.has_text(start + Duration::from_secs(1)));
        m.expire(start + CLEARS_AFTER);
        assert!(m.held.is_none());
    }
}
