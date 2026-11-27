//! One test per way a held request ends (T015). A timeout or a failure is never an Allow.

use pocketgpu_proto::assistants::{
    Answer, ApprovalState, AskWhy, ENDED_KEEP_MS, Ended, Kind, OPEN_MAX, REMEMBER_ALLOW_MS,
};
use tokio::sync::oneshot::error::TryRecvError;

use super::*;

const T0: i64 = 1_000_000;

fn new(why: AskWhy, session: &str, cmd: &str) -> NewRequest {
    NewRequest {
        why,
        kind: if why == AskWhy::AskFirst {
            Kind::Delete
        } else {
            Kind::Other
        },
        assistant: "claude".into(),
        session: session.into(),
        permission_mode: "default".into(),
        tool: "Bash".into(),
        detail_text: cmd.into(),
        input_hash: input_hash("Bash", cmd),
        folder: "/work".into(),
        summary: cmd.into(),
    }
}

fn held(o: Opened) -> Held {
    match o {
        Opened::Held(h) => h,
        Opened::Ended(a, _) => panic!("not held: {a:?}"),
    }
}

fn got(h: &mut Held) -> Answer {
    h.rx.try_recv().expect("an answer")
}

fn waiting(h: &mut Held) -> bool {
    matches!(h.rx.try_recv(), Err(TryRecvError::Empty))
}

#[test]
fn allow_from_the_phone() {
    let a = Approvals::default();
    let mut h = held(a.open(new(AskWhy::AskFirst, "s1", "rm -rf build"), T0));
    assert!(waiting(&mut h));
    let (view, done) = a.answer(h.id, T0, "dev1", "Pixel", true, T0 + 5).unwrap();
    assert_eq!(got(&mut h), Answer::Allow);
    assert_eq!(view.state, ApprovalState::Allowed);
    assert_eq!(view.answered_by_name.as_deref(), Some("Pixel"));
    assert_eq!(done.answered_by.as_deref(), Some("dev1"));
    assert_eq!(done.target, "claude · s1 · delete · rm -rf build");
}

#[test]
fn deny_from_the_phone() {
    let a = Approvals::default();
    let mut h = held(a.open(new(AskWhy::AskOnPhone, "s1", "ls"), T0));
    let (view, done) = a.answer(h.id, T0, "dev1", "Pixel", false, T0).unwrap();
    assert_eq!(
        got(&mut h),
        Answer::Deny {
            message: DENIED.into()
        }
    );
    assert_eq!(view.state, ApprovalState::Denied);
    assert_eq!(done.state, ApprovalState::Denied);
}

#[test]
fn answered_at_the_computer() {
    let a = Approvals::default();
    let mut one = held(a.open(new(AskWhy::AskOnPhone, "s1", "ls"), T0));
    let mut two = held(a.open(new(AskWhy::AskOnPhone, "s1", "pwd"), T0));
    let done = a.desk(
        "claude",
        "s1",
        Desk::ToolDone(input_hash("Bash", "ls")),
        T0 + 1,
    );
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].state, ApprovalState::AnsweredAtComputer);
    assert_eq!(done[0].answered_by, None);
    assert_eq!(
        got(&mut one),
        Answer::NoDecision {
            why: Ended::AnsweredAtComputer
        }
    );
    assert!(
        waiting(&mut two),
        "the other call in the session still waits"
    );
    let err = a
        .answer(one.id, T0, "dev1", "Pixel", true, T0 + 2)
        .unwrap_err();
    assert_eq!(err, "already answered at the computer");
    // The session's Stop or next prompt ends the rest.
    assert_eq!(a.desk("claude", "s1", Desk::TurnOver, T0 + 3).len(), 1);
    assert_eq!(
        got(&mut two),
        Answer::NoDecision {
            why: Ended::AnsweredAtComputer
        }
    );
}

#[test]
fn no_phone_reached() {
    let a = Approvals::default();
    let mut ask = held(a.open(new(AskWhy::AskOnPhone, "s1", "ls"), T0));
    let done = a.end(ask.id, Ended::NoPhone, T0).unwrap();
    assert_eq!(done.state, ApprovalState::NoPhone);
    assert_eq!(
        got(&mut ask),
        Answer::NoDecision {
            why: Ended::NoPhone
        }
    );

    // "Ask me first": the desk prompt only where one shows, else a deny.
    let mut first = held(a.open(new(AskWhy::AskFirst, "s1", "rm a"), T0));
    a.end(first.id, Ended::NoPhone, T0);
    assert_eq!(
        got(&mut first),
        Answer::NoDecision {
            why: Ended::NoPhone
        }
    );
    let mut auto = new(AskWhy::AskFirst, "s1", "rm b");
    auto.permission_mode = "bypassPermissions".into();
    let mut auto = held(a.open(auto, T0));
    a.end(auto.id, Ended::NoPhone, T0);
    assert_eq!(
        got(&mut auto),
        Answer::Deny {
            message: COULDNT_ASK.into()
        }
    );
}

#[test]
fn fail_closed_depends_on_the_mode() {
    for mode in ["default", "plan"] {
        assert!(matches!(
            fail_closed("claude", mode, Ended::NoPhone),
            Answer::NoDecision { .. }
        ));
    }
    for mode in [
        "auto",
        "acceptEdits",
        "dontAsk",
        "bypassPermissions",
        "",
        "new-mode",
    ] {
        assert_eq!(
            fail_closed("claude", mode, Ended::NoPhone),
            Answer::Deny {
                message: COULDNT_ASK.into()
            },
            "{mode}"
        );
    }
    assert!(matches!(
        fail_closed("cursor", "", Ended::NoPhone),
        Answer::NoDecision { .. }
    ));
}

#[test]
fn timed_out() {
    let a = Approvals::default();
    let mut ask = held(a.open(new(AskWhy::AskOnPhone, "s1", "ls"), T0));
    let mut first = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    let mut auto = new(AskWhy::AskFirst, "s2", "rm x");
    auto.permission_mode = "auto".into();
    let mut auto = held(a.open(auto, T0));
    for h in [&ask, &first, &auto] {
        assert_eq!(
            a.end(h.id, Ended::TimedOut, T0 + 300_000).unwrap().state,
            ApprovalState::TimedOut
        );
    }
    assert_eq!(
        got(&mut ask),
        Answer::NoDecision {
            why: Ended::TimedOut
        }
    );
    let no = Answer::Deny {
        message: NO_ANSWER.into(),
    };
    assert_eq!(got(&mut first), no);
    assert_eq!(got(&mut auto), no);
    // A timer firing after the end does nothing.
    assert_eq!(a.end(ask.id, Ended::TimedOut, T0 + 300_001), None);
}

#[test]
fn hook_gone() {
    let a = Approvals::default();
    let h = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    let done = a.leave(h.id, h.waiter, T0 + 1).unwrap();
    assert_eq!(done.state, ApprovalState::NoLongerNeeded);
    assert_eq!(a.get(h.id).unwrap().state, ApprovalState::NoLongerNeeded);
    assert_eq!(
        a.answer(h.id, T0, "d", "P", true, T0 + 2).unwrap_err(),
        "no longer open"
    );
}

#[test]
fn a_second_answer_is_refused() {
    let a = Approvals::default();
    let mut h = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    a.answer(h.id, T0, "dev1", "Pixel", false, T0).unwrap();
    assert_eq!(
        a.answer(h.id, T0, "dev2", "iPhone", true, T0).unwrap_err(),
        "already denied on Pixel"
    );
    assert_eq!(
        got(&mut h),
        Answer::Deny {
            message: DENIED.into()
        }
    );
    let h2 = held(a.open(new(AskWhy::AskFirst, "s1", "rm y"), T0));
    a.answer(h2.id, T0, "dev1", "Pixel", true, T0).unwrap();
    assert_eq!(
        a.answer(h2.id, T0, "dev2", "iPhone", false, T0)
            .unwrap_err(),
        "already allowed on Pixel"
    );
}

#[test]
fn a_stale_id_or_wrong_opened_ms_is_refused() {
    let a = Approvals::default();
    let mut h = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    assert_eq!(
        a.answer(h.id ^ 1, T0, "d", "P", true, T0).unwrap_err(),
        "no longer open"
    );
    assert_eq!(
        a.answer(h.id, T0 + 1, "d", "P", true, T0).unwrap_err(),
        "no longer open"
    );
    assert!(waiting(&mut h), "a refused answer changes nothing");
    // From an agent run before a restart: a fresh store knows no id.
    let fresh = Approvals::default();
    assert!(fresh.answer(h.id, T0, "d", "P", true, T0).is_err());
}

#[test]
fn too_long_can_only_be_denied() {
    let a = Approvals::default();
    let long = format!(
        "echo {}",
        "x".repeat(pocketgpu_proto::assistants::DETAIL_MAX)
    );
    let mut h = held(a.open(new(AskWhy::AskOnPhone, "s1", &long), T0));
    let view = a.get(h.id).unwrap();
    assert!(view.truncated);
    assert_eq!(view.detail.len(), pocketgpu_proto::assistants::DETAIL_MAX);
    assert_eq!(
        a.answer(h.id, T0, "d", "P", true, T0).unwrap_err(),
        TOO_LONG
    );
    assert!(waiting(&mut h));
    a.answer(h.id, T0, "d", "P", false, T0).unwrap();
}

#[test]
fn parallel_requests_end_on_their_own() {
    let a = Approvals::default();
    let mut one = held(a.open(new(AskWhy::AskFirst, "s1", "rm a"), T0));
    let mut two = held(a.open(new(AskWhy::AskFirst, "s1", "rm b"), T0));
    assert_ne!(one.id, two.id);
    a.answer(two.id, T0, "d", "P", false, T0).unwrap();
    assert!(waiting(&mut one));
    a.end(one.id, Ended::TimedOut, T0 + 300_000);
    assert_eq!(
        got(&mut two),
        Answer::Deny {
            message: DENIED.into()
        }
    );
    assert_eq!(
        got(&mut one),
        Answer::Deny {
            message: NO_ANSWER.into()
        }
    );
}

#[test]
fn a_sibling_event_never_ends_ask_first() {
    let a = Approvals::default();
    let mut h = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    let mut other = held(a.open(new(AskWhy::AskOnPhone, "s2", "ls"), T0));
    assert!(
        a.desk(
            "claude",
            "s1",
            Desk::ToolDone(input_hash("Bash", "rm x")),
            T0
        )
        .is_empty()
    );
    assert!(a.desk("claude", "s1", Desk::TurnOver, T0).is_empty());
    assert!(
        a.desk("codex", "s2", Desk::TurnOver, T0).is_empty(),
        "another assistant"
    );
    assert!(waiting(&mut h));
    assert!(waiting(&mut other));
}

#[test]
fn a_joined_duplicate_gets_the_same_answer() {
    let a = Approvals::default();
    let mut first = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    let mut again = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0 + 1));
    assert!(again.joined && !first.joined);
    assert_eq!(again.id, first.id);
    assert_eq!(a.list(T0).len(), 1);
    // One hook leaving doesn't end it while the other waits.
    assert_eq!(a.leave(first.id, first.waiter, T0 + 2), None);
    assert!(matches!(first.rx.try_recv(), Err(TryRecvError::Closed)));
    a.answer(again.id, T0, "d", "P", true, T0 + 3).unwrap();
    assert_eq!(got(&mut again), Answer::Allow);
    // Another session's identical call is its own request.
    let other = held(a.open(new(AskWhy::AskFirst, "s2", "rm x"), T0));
    assert!(!other.joined);
}

#[test]
fn a_remembered_allow_is_used_once() {
    let a = Approvals::default();
    let hash = input_hash("Bash", "rm x");
    let h = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    assert!(
        !a.take_remembered("claude", "s1", &hash, T0),
        "nothing before the Allow"
    );
    a.answer(h.id, T0, "d", "P", true, T0).unwrap();
    assert!(
        !a.take_remembered("claude", "s2", &hash, T0 + 1),
        "another session"
    );
    assert!(a.take_remembered("claude", "s1", &hash, T0 + 1));
    assert!(
        !a.take_remembered("claude", "s1", &hash, T0 + 2),
        "used once"
    );
    // A Deny is never remembered, and an Allow is forgotten after a minute.
    let d = held(a.open(new(AskWhy::AskFirst, "s1", "rm y"), T0));
    a.answer(d.id, T0, "d", "P", false, T0).unwrap();
    assert!(!a.take_remembered("claude", "s1", &input_hash("Bash", "rm y"), T0));
    let late = held(a.open(new(AskWhy::AskFirst, "s1", "rm z"), T0));
    a.answer(late.id, T0, "d", "P", true, T0).unwrap();
    let z = input_hash("Bash", "rm z");
    assert!(!a.take_remembered("claude", "s1", &z, T0 + REMEMBER_ALLOW_MS));
}

#[test]
fn only_this_runs_ask_first_allow_is_remembered() {
    let a = Approvals::default();
    let hash = input_hash("Bash", "rm x");
    let p = held(a.open(new(AskWhy::AskOnPhone, "s1", "rm x"), T0));
    a.answer(p.id, T0, "d", "P", true, T0).unwrap();
    assert!(
        !a.take_remembered("claude", "s1", &hash, T0),
        "ask on phone"
    );
    let first = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    a.answer(first.id, T0, "d", "P", true, T0).unwrap();
    // The next run opens before its desk prompt: run 1's Allow is gone.
    let second = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0 + 1));
    a.end(second.id, Ended::NoPhone, T0 + 2);
    assert!(!a.take_remembered("claude", "s1", &hash, T0 + 3));
}

#[test]
fn too_many_open_end_as_no_phone() {
    let a = Approvals::default();
    for n in 0..OPEN_MAX {
        held(a.open(new(AskWhy::AskFirst, "s1", &format!("rm {n}")), T0));
    }
    match a.open(new(AskWhy::AskOnPhone, "s1", "rm more"), T0) {
        Opened::Ended(answer, done) => {
            assert_eq!(
                answer,
                Answer::NoDecision {
                    why: Ended::NoPhone
                }
            );
            assert_eq!(done.state, ApprovalState::NoPhone);
        }
        Opened::Held(_) => panic!("held past the limit"),
    }
}

#[test]
fn ended_requests_are_kept_ten_minutes() {
    let a = Approvals::default();
    let h = held(a.open(new(AskWhy::AskOnPhone, "s1", "ls"), T0));
    let open = a.list(T0);
    assert_eq!(open[0].state, ApprovalState::Waiting);
    assert_eq!(open[0].ends_ms, T0 + 300_000);
    assert!(!open[0].risky, "an ordinary desk question");
    a.end(h.id, Ended::NoPhone, T0);
    assert_eq!(a.list(T0 + ENDED_KEEP_MS - 1).len(), 1);
    assert!(a.list(T0 + ENDED_KEEP_MS).is_empty());
    let r = held(a.open(new(AskWhy::AskFirst, "s1", "rm x"), T0));
    assert!(a.get(r.id).unwrap().risky);
}
