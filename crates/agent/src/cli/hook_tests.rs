//! `pgpu hook` mapping, with inputs as Claude Code, Codex and Cursor send them (their docs'
//! own examples, in `tests/fixtures/hooks`).

use super::*;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/hooks/", $name, ".json"))
    };
}

fn seen(session: &str, event: Event) -> Option<Seen> {
    Some(Seen {
        session: session.into(),
        event,
    })
}

#[test]
fn claude_code_events() {
    assert_eq!(
        parse(fixture!("claude_user_prompt_submit")),
        seen("abc123", Event::TurnStarted)
    );
    assert_eq!(
        parse(fixture!("claude_notification_permission")),
        seen("abc123", Event::NeedsYou)
    );
    assert_eq!(
        parse(fixture!("claude_notification_idle")),
        seen("abc123", Event::Idle)
    );
    assert_eq!(parse(fixture!("claude_notification_auth")), None);
    assert_eq!(
        parse(fixture!("claude_post_tool_use")),
        seen("abc123", Event::Working)
    );
    assert_eq!(
        parse(fixture!("claude_stop")),
        seen("abc123", Event::TurnEnded)
    );
}

#[test]
fn codex_events() {
    let session = "0199a213-81c0-7800-8aa1-bbab2a035a53";
    assert_eq!(
        parse(fixture!("codex_user_prompt_submit")),
        seen(session, Event::TurnStarted)
    );
    assert_eq!(
        parse(fixture!("codex_stop")),
        seen(session, Event::TurnEnded)
    );
    // The notify program: the JSON arrives as the last argument.
    let arg: OsString = fixture!("codex_notify").trim().into();
    let text = json_arg(&[arg]).unwrap();
    assert_eq!(
        parse(&text),
        seen("b5f6c1c2-1111-2222-3333-444455556666", Event::TurnEnded)
    );
}

#[test]
fn cursor_events() {
    let session = "c0ffee00-aaaa-bbbb-cccc-1234567890ab";
    assert_eq!(
        parse(fixture!("cursor_before_submit_prompt")),
        seen(session, Event::TurnStarted)
    );
    assert_eq!(
        parse(fixture!("cursor_stop")),
        seen(session, Event::TurnEnded)
    );
    assert_eq!(parse(fixture!("cursor_stop_aborted")), None);
}

#[test]
fn nothing_the_assistant_wrote_is_kept() {
    // Only the event and the session id come out, whatever the input holds.
    for input in [
        fixture!("claude_user_prompt_submit"),
        fixture!("claude_stop"),
        fixture!("claude_post_tool_use"),
        fixture!("codex_notify"),
        fixture!("cursor_before_submit_prompt"),
    ] {
        let s = parse(input).unwrap();
        let out = format!("{s:?}");
        for secret in [
            "factorial",
            "refactoring",
            "Rename",
            "failing test",
            "/Users",
            "file content",
            "file.txt",
        ] {
            assert!(!out.contains(secret), "{out}");
        }
    }
}

#[test]
fn unknown_or_broken_input_is_ignored() {
    for input in [
        "",
        "not json",
        "[]",
        "{}",
        r#"{"hook_event_name":"PreToolUse","session_id":"s"}"#,
        r#"{"hook_event_name":"SubagentStop","session_id":"s"}"#,
        r#"{"hook_event_name":"Notification","session_id":"s"}"#,
        r#"{"type":"something-else","thread-id":"t"}"#,
        r#"{"hook_event_name":42}"#,
    ] {
        assert_eq!(parse(input), None, "{input:?}");
    }
    // No session id still counts; the agent keeps it as one unnamed session.
    assert_eq!(
        parse(r#"{"hook_event_name":"Stop"}"#),
        seen("", Event::TurnEnded)
    );
    let long = format!(
        r#"{{"hook_event_name":"Stop","session_id":"{}"}}"#,
        "x".repeat(500)
    );
    assert_eq!(parse(&long).unwrap().session.len(), SESSION_MAX);
}

#[test]
fn a_huge_prompt_cut_at_the_limit_still_counts() {
    let full = format!(
        r#"{{"session_id":"abc123","hook_event_name":"UserPromptSubmit","prompt":"{}"}}"#,
        "log line\\n".repeat(20_000)
    );
    let cut = &full[..MAX_INPUT];
    assert_eq!(parse(cut), seen("abc123", Event::TurnStarted));
}

#[test]
fn only_a_json_last_argument_is_taken() {
    assert_eq!(json_arg(&[]), None);
    assert_eq!(json_arg(&["--quiet".into()]), None);
    assert_eq!(
        json_arg(&["x".into(), r#" {"type":"t"}"#.into()]).as_deref(),
        Some(r#" {"type":"t"}"#)
    );
    let huge: OsString = format!("{{{}}}", " ".repeat(MAX_INPUT)).into();
    assert_eq!(json_arg(&[huge]), None);
}
