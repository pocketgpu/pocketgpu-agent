//! The rules in `tracker.rs`, walking the clock through hours at once.

use super::*;
use crate::assistants::limits::{ALL_PER_HOUR, PER_HOUR};

const MIN: i64 = 60_000;

#[test]
fn a_long_turn_ends_with_finished_and_a_short_one_quietly() {
    let mut t = Tracker::default();
    assert_eq!(
        t.event("claude", "s1", Event::TurnStarted, 0),
        Decision::Nothing
    );
    assert_eq!(
        t.event("claude", "s1", Event::TurnEnded, 40 * MIN + 30_000),
        Decision::Send(Alert::Finished { minutes: 40 })
    );
    t.event("claude", "s1", Event::TurnStarted, 50 * MIN);
    assert_eq!(
        t.event("claude", "s1", Event::TurnEnded, 59 * MIN),
        Decision::Nothing,
        "nine minutes is not long"
    );
    // Exactly ten minutes is long enough.
    t.event("claude", "s1", Event::TurnStarted, 60 * MIN);
    assert_eq!(
        t.event("claude", "s1", Event::TurnEnded, 70 * MIN),
        Decision::Send(Alert::Finished { minutes: 10 })
    );
}

#[test]
fn an_unknown_start_sends_nothing() {
    let mut t = Tracker::default();
    assert_eq!(
        t.event("codex", "s", Event::TurnEnded, 90 * MIN),
        Decision::Nothing
    );
    // A turn counts once: a second end has no start either.
    t.event("codex", "s", Event::TurnStarted, 0);
    t.event("codex", "s", Event::TurnEnded, 20 * MIN);
    assert_eq!(
        t.event("codex", "s", Event::TurnEnded, 40 * MIN),
        Decision::Nothing
    );
}

#[test]
fn sessions_and_assistants_keep_their_own_turns() {
    let mut t = Tracker::default();
    t.event("claude", "a", Event::TurnStarted, 0);
    t.event("claude", "b", Event::TurnStarted, 15 * MIN);
    t.event("codex", "a", Event::TurnStarted, 5 * MIN);
    assert_eq!(
        t.event("claude", "b", Event::TurnEnded, 20 * MIN),
        Decision::Nothing
    );
    assert_eq!(
        t.event("claude", "a", Event::TurnEnded, 20 * MIN),
        Decision::Send(Alert::Finished { minutes: 20 })
    );
    assert_eq!(
        t.event("codex", "a", Event::TurnEnded, 20 * MIN),
        Decision::Send(Alert::Finished { minutes: 15 })
    );
}

#[test]
fn a_question_waits_and_is_dropped_when_the_user_answers() {
    let mut t = Tracker::default();
    t.event("claude", "s", Event::TurnStarted, 0);
    let Decision::Wait { mark } = t.event("claude", "s", Event::NeedsYou, MIN) else {
        panic!("a permission question waits first");
    };
    // The user answered at the desk: the next turn started.
    t.event("claude", "s", Event::TurnStarted, MIN + 20_000);
    assert_eq!(t.wait_over("claude", "s", mark, 3 * MIN), None);

    let Decision::Wait { mark } = t.event("claude", "s", Event::NeedsYou, 4 * MIN) else {
        panic!("waits");
    };
    assert_eq!(
        t.wait_over("claude", "s", mark, 6 * MIN),
        Some(Alert::NeedsYou)
    );
    // Another session's events don't count as an answer here.
    let Decision::Wait { mark } = t.event("claude", "s2", Event::NeedsYou, 7 * MIN) else {
        panic!("waits");
    };
    t.event("claude", "s", Event::TurnStarted, 8 * MIN);
    assert_eq!(
        t.wait_over("claude", "s2", mark, 9 * MIN),
        Some(Alert::NeedsYou)
    );
}

#[test]
fn a_tool_running_after_a_question_means_it_was_answered() {
    let mut t = Tracker::default();
    t.event("claude", "s", Event::TurnStarted, 0);
    let Decision::Wait { mark } = t.event("claude", "s", Event::NeedsYou, MIN) else {
        panic!("waits");
    };
    // Approved at the desk: Claude Code says nothing, but the tool runs and reports.
    assert_eq!(
        t.event("claude", "s", Event::Working, MIN + 30_000),
        Decision::Nothing
    );
    assert_eq!(t.wait_over("claude", "s", mark, 3 * MIN), None);
    // Work in the same turn doesn't end it or count as a "finished".
    assert_eq!(
        t.event("claude", "s", Event::TurnEnded, 15 * MIN),
        Decision::Send(Alert::Finished { minutes: 15 })
    );
}

#[test]
fn needs_you_at_most_once_per_session_in_ten_minutes() {
    let mut t = Tracker::default();
    assert_eq!(
        t.event("claude", "s", Event::Idle, 0),
        Decision::Send(Alert::NeedsYou),
        "idle sends at once"
    );
    assert_eq!(
        t.event("claude", "s", Event::Idle, 5 * MIN),
        Decision::Nothing
    );
    let Decision::Wait { mark } = t.event("claude", "s", Event::NeedsYou, 6 * MIN) else {
        panic!("waits");
    };
    assert_eq!(t.wait_over("claude", "s", mark, 8 * MIN), None);
    // Another session may still say so.
    assert_eq!(
        t.event("claude", "other", Event::Idle, 9 * MIN),
        Decision::Send(Alert::NeedsYou)
    );
    assert_eq!(
        t.event("claude", "s", Event::Idle, 10 * MIN),
        Decision::Send(Alert::NeedsYou)
    );
}

#[test]
fn no_idle_reminder_right_after_finished() {
    let mut t = Tracker::default();
    t.event("claude", "s", Event::TurnStarted, 0);
    assert!(matches!(
        t.event("claude", "s", Event::TurnEnded, 30 * MIN),
        Decision::Send(Alert::Finished { .. })
    ));
    assert_eq!(
        t.event("claude", "s", Event::Idle, 31 * MIN),
        Decision::Nothing
    );
    // A one-minute turn: the user was at the desk and is reading the answer.
    t.event("claude", "s", Event::TurnStarted, 32 * MIN);
    t.event("claude", "s", Event::TurnEnded, 33 * MIN);
    assert_eq!(
        t.event("claude", "s", Event::Idle, 34 * MIN),
        Decision::Nothing
    );
    // After five minutes they may have walked away: idle counts again.
    t.event("claude", "s", Event::TurnStarted, 40 * MIN);
    t.event("claude", "s", Event::TurnEnded, 45 * MIN);
    assert_eq!(
        t.event("claude", "s", Event::Idle, 46 * MIN),
        Decision::Send(Alert::NeedsYou)
    );
}

#[test]
fn idle_after_a_turn_of_unknown_length_counts() {
    let mut t = Tracker::default();
    // The agent restarted during the turn: its start was never seen.
    t.event("claude", "s", Event::TurnEnded, 10 * MIN);
    assert_eq!(
        t.event("claude", "s", Event::Idle, 11 * MIN),
        Decision::Send(Alert::NeedsYou)
    );
    // A turn started but its end never came: the last length no longer applies.
    let mut t = Tracker::default();
    t.event("claude", "s", Event::TurnStarted, 0);
    t.event("claude", "s", Event::TurnEnded, MIN);
    t.event("claude", "s", Event::TurnStarted, 2 * MIN);
    assert_eq!(
        t.event("claude", "s", Event::Idle, 20 * MIN),
        Decision::Send(Alert::NeedsYou)
    );
}

#[test]
fn at_most_ten_pushes_an_hour_per_assistant() {
    let mut t = Tracker::default();
    for i in 0..PER_HOUR as i64 {
        let d = t.event("claude", &format!("s{i}"), Event::Idle, i * MIN);
        assert_eq!(d, Decision::Send(Alert::NeedsYou), "push {i}");
    }
    assert_eq!(
        t.event("claude", "s99", Event::Idle, 30 * MIN),
        Decision::Nothing
    );
    t.event("claude", "long", Event::TurnStarted, 0);
    assert_eq!(
        t.event("claude", "long", Event::TurnEnded, 40 * MIN),
        Decision::Nothing
    );
    // Other assistants have their own count.
    assert_eq!(
        t.event("codex", "s", Event::Idle, 30 * MIN),
        Decision::Send(Alert::NeedsYou)
    );
    // An hour after the first push, there is room again.
    assert_eq!(
        t.event("claude", "s98", Event::Idle, 60 * MIN),
        Decision::Send(Alert::NeedsYou)
    );
}

#[test]
fn made_up_names_cant_open_new_counts() {
    let mut t = Tracker::default();
    let sent = (1..=30)
        .filter(|i| t.event(&format!("a{i}"), "s", Event::Idle, *i) != Decision::Nothing)
        .count();
    assert!(sent <= ALL_PER_HOUR);
    assert_eq!(sent, PER_HOUR, "every other name shares one count");
    // Notes are counted for the whole agent, and in the same total.
    let notes = (0..30).filter(|i| t.allow_note(MIN + i)).count();
    assert_eq!(notes, ALL_PER_HOUR - PER_HOUR);
}

#[test]
fn old_sessions_are_forgotten_and_the_count_is_capped() {
    let mut t = Tracker::default();
    t.event("claude", "old", Event::TurnStarted, 0);
    t.event("claude", "new", Event::TurnStarted, 25 * 60 * MIN);
    assert_eq!(
        t.sessions.len(),
        1,
        "a day without events forgets a session"
    );
    assert_eq!(
        t.event("claude", "old", Event::TurnEnded, 25 * 60 * MIN + MIN),
        Decision::Nothing
    );
    let mut t = Tracker::default();
    for i in 0..(MAX_SESSIONS as i64 + 20) {
        t.event("claude", &format!("s{i}"), Event::TurnStarted, i);
    }
    assert_eq!(t.sessions.len(), MAX_SESSIONS);
    assert!(!t.sessions.contains_key(&("claude".into(), "s0".into())));
}
