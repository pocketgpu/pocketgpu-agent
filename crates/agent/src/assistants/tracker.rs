//! When an AI assistant's hook events become a push (F32). Plain rules with the clock passed
//! in, so tests can walk through hours at once; `super` does the waiting and the sending.

use std::collections::HashMap;

use pocketgpu_proto::local::AssistantEventKind as Event;

use super::limits::Limits;

/// How long a permission question waits before the phone hears of it: people at the desk
/// usually answer well before. Also the shortest turn after which an idle prompt may mean
/// the user walked away; after a shorter one they are still reading the answer.
pub const NEEDS_YOU_WAIT_MS: i64 = 120_000;
/// A turn this long or longer earns a "finished"; shorter ones end while people still wait.
pub const LONG_TURN_MS: i64 = 10 * 60_000;
/// At most one "needs you" per session in this long, so a chatty session can't nag.
pub const NEEDS_YOU_GAP_MS: i64 = 10 * 60_000;
const HOUR_MS: i64 = 3_600_000;
/// A session with no events for a day is forgotten; at most this many are kept at once.
const FORGET_MS: i64 = 24 * HOUR_MS;
const MAX_SESSIONS: usize = 256;

/// A push about an assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    NeedsYou,
    /// The turn took this many whole minutes.
    Finished {
        minutes: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Nothing,
    Send(Alert),
    /// Call [`Tracker::wait_over`] with this mark after [`NEEDS_YOU_WAIT_MS`].
    Wait {
        mark: u64,
    },
}

#[derive(Debug, Default)]
struct Session {
    turn_started_ms: Option<i64>,
    /// How long the last turn took, when its start and end were both seen.
    last_turn_ms: Option<i64>,
    last_needs_you_ms: Option<i64>,
    /// "Finished" went out for the last turn: the idle reminder that follows adds nothing.
    finished_sent: bool,
    /// Set by every event, so a waiting "needs you" can tell that the user answered.
    mark: u64,
    seen_ms: i64,
}

/// Per (assistant, session): when the turn started and what the phone already heard.
#[derive(Debug, Default)]
pub struct Tracker {
    sessions: HashMap<(String, String), Session>,
    limits: Limits,
    last_mark: u64,
}

impl Tracker {
    /// What to do about one event. `assistant` is the key for limits (its short name).
    pub fn event(&mut self, assistant: &str, session: &str, event: Event, now: i64) -> Decision {
        self.forget_old(now);
        self.last_mark += 1;
        let mark = self.last_mark;
        let key = (assistant.to_string(), session.to_string());
        if !self.sessions.contains_key(&key) && self.sessions.len() >= MAX_SESSIONS {
            self.forget_oldest();
        }
        let s = self.sessions.entry(key).or_default();
        s.mark = mark;
        s.seen_ms = now;
        match event {
            Event::TurnStarted => {
                s.turn_started_ms = Some(now);
                s.last_turn_ms = None;
                s.finished_sent = false;
                Decision::Nothing
            }
            // Only a sign of life: the new mark is enough to drop a waiting "needs you".
            Event::Working => Decision::Nothing,
            Event::NeedsYou => Decision::Wait { mark },
            Event::Idle if s.finished_sent => Decision::Nothing,
            // Claude Code says it is idle a minute after every answer the user hasn't replied
            // to yet. After a short turn they were at the desk and are reading it; only a
            // turn long enough to walk away from (or one whose length is unknown) counts.
            Event::Idle if s.last_turn_ms.is_some_and(|t| t < NEEDS_YOU_WAIT_MS) => {
                Decision::Nothing
            }
            Event::Idle => match needs_you(s, &mut self.limits, assistant, now) {
                Some(a) => Decision::Send(a),
                None => Decision::Nothing,
            },
            Event::TurnEnded => {
                // With no start seen (the agent restarted, or the hook came late) the length
                // is unknown, and a "finished" might come after a short turn.
                let Some(start) = s.turn_started_ms.take() else {
                    s.last_turn_ms = None;
                    return Decision::Nothing;
                };
                let took = now.saturating_sub(start);
                s.last_turn_ms = Some(took);
                if took < LONG_TURN_MS || !self.limits.allow(assistant, now) {
                    return Decision::Nothing;
                }
                s.finished_sent = true;
                let minutes = u32::try_from(took / 60_000).unwrap_or(u32::MAX);
                Decision::Send(Alert::Finished { minutes })
            }
        }
    }

    /// After the wait for a permission question: "needs you", unless anything else happened
    /// in that session since (the user answered at the desk).
    pub fn wait_over(
        &mut self,
        assistant: &str,
        session: &str,
        mark: u64,
        now: i64,
    ) -> Option<Alert> {
        let key = (assistant.to_string(), session.to_string());
        let s = self.sessions.get_mut(&key)?;
        if s.mark != mark {
            return None;
        }
        needs_you(s, &mut self.limits, assistant, now)
    }

    /// Whether a note from `pgpu mcp` may go out now; counts it if so. Notes from every
    /// session share one count ([`super::limits::NOTES_PER_HOUR`]).
    pub fn allow_note(&mut self, now: i64) -> bool {
        self.limits.allow_note(now)
    }

    fn forget_old(&mut self, now: i64) {
        self.sessions
            .retain(|_, s| now.saturating_sub(s.seen_ms) < FORGET_MS);
        self.limits.forget_old(now);
    }

    fn forget_oldest(&mut self) {
        let oldest = self
            .sessions
            .iter()
            .min_by_key(|(_, s)| s.seen_ms)
            .map(|(k, _)| k.clone());
        if let Some(k) = oldest {
            self.sessions.remove(&k);
        }
    }
}

fn needs_you(s: &mut Session, limits: &mut Limits, assistant: &str, now: i64) -> Option<Alert> {
    if s.last_needs_you_ms
        .is_some_and(|t| now.saturating_sub(t) < NEEDS_YOU_GAP_MS)
    {
        return None;
    }
    if !limits.allow(assistant, now) {
        return None;
    }
    s.last_needs_you_ms = Some(now);
    Some(Alert::NeedsYou)
}

#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tests;
