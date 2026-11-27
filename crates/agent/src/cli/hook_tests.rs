//! `pgpu hook` mapping, with inputs as Claude Code, Codex, Cursor and Gemini CLI send them
//! (their docs' own examples, in `tests/fixtures/hooks`).

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
    // A finished or failed tool call carries the hash of that call, nothing it wrote.
    let done = |input: &str| {
        let call = hook_answer::tool_call(input).unwrap();
        Event::ToolDone {
            input_hash: call.hash(),
        }
    };
    let post = fixture!("claude_post_tool_use");
    assert_eq!(parse(post), seen("abc123", done(post)));
    let failed = fixture!("claude_post_tool_use_failure");
    assert_eq!(parse(failed), seen("abc123", done(failed)));
    // Without a tool it's still a sign of life.
    assert_eq!(
        parse(r#"{"hook_event_name":"PostToolUse","session_id":"abc123"}"#),
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
fn gemini_cli_events() {
    let session = "7f3c2a10-5e4b-4c1d-9a8e-0b1c2d3e4f50";
    assert_eq!(
        parse(fixture!("gemini_before_agent")),
        seen(session, Event::TurnStarted)
    );
    assert_eq!(
        parse(fixture!("gemini_notification_permission")),
        seen(session, Event::NeedsYou)
    );
    // A tool ran: only a sign of life, never the tool call's text or its hash.
    assert_eq!(
        parse(fixture!("gemini_after_tool")),
        seen(session, Event::Working)
    );
    assert_eq!(
        parse(fixture!("gemini_after_agent")),
        seen(session, Event::TurnEnded)
    );
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

mod ask {
    use super::*;
    use crate::assistants::answers::input_hash;
    use pocketgpu_proto::assistants::{Answer, Ended};
    use pocketgpu_proto::local::Response;
    use serde_json::{Value, json};

    #[test]
    fn modes_come_from_the_flags() {
        assert_eq!(mode(&[]), Mode::Plain);
        assert_eq!(mode(&["--ask".into()]), Mode::Ask);
        assert_eq!(mode(&["--check".into()]), Mode::Check);
        assert_eq!(mode(&[r#"{"type":"t"}"#.into()]), Mode::Plain);
    }

    #[test]
    fn the_permission_request_is_read_whole() {
        let call = hook_answer::tool_call(fixture!("claude_permission_request")).unwrap();
        assert_eq!(call.event, "PermissionRequest");
        assert_eq!(call.session, "abc123");
        assert_eq!(call.permission_mode, "default");
        assert_eq!(call.tool, "Bash");
        assert_eq!(call.folder, "/Users/.../my-project");
        let input: Value = serde_json::from_str(&call.input).unwrap();
        assert_eq!(input["command"], "rm -rf node_modules");
        // The agent hashes what the hook sends; the finished call hashes the same.
        assert_eq!(call.hash(), input_hash("Bash", &call.input));
        let failed = hook_answer::tool_call(fixture!("claude_post_tool_use_failure")).unwrap();
        assert_eq!(failed.hash(), call.hash());
        let other = hook_answer::tool_call(fixture!("claude_post_tool_use")).unwrap();
        assert_ne!(other.hash(), call.hash());
        for bad in [
            "",
            "not json",
            "[]",
            r#"{"hook_event_name":"PermissionRequest"}"#,
        ] {
            assert_eq!(hook_answer::tool_call(bad), None, "{bad:?}");
        }
    }

    fn printed(answer: Answer) -> Option<Value> {
        hook_answer::claude_permission_request(&answer).map(|line| {
            assert!(!line.contains('\n'), "one line: {line}");
            serde_json::from_str(&line).unwrap()
        })
    }

    #[test]
    fn claude_code_gets_the_phones_allow_or_deny() {
        assert_eq!(
            printed(Answer::Allow),
            Some(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "allow" }
                }
            }))
        );
        assert_eq!(
            printed(Answer::Deny {
                message: "Denied on your phone".into()
            }),
            Some(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "deny", "message": "Denied on your phone" }
                }
            }))
        );
    }

    #[test]
    fn nothing_is_printed_without_a_phone_answer() {
        assert_eq!(printed(Answer::Pass), None);
        for why in [
            Ended::AnsweredAtComputer,
            Ended::NoPhone,
            Ended::TimedOut,
            Ended::NoLongerNeeded,
        ] {
            assert_eq!(printed(Answer::NoDecision { why }), None, "{why:?}");
        }
    }

    #[test]
    fn only_a_whole_answer_counts() {
        use hook_answer::final_answer;
        let pass = Response::AssistantAnswer(Answer::Pass);
        let allow = Response::AssistantAnswer(Answer::Allow);
        let held = Response::AssistantHeld { id: 7 };
        assert_eq!(final_answer(Ok((pass, None))), Some(Answer::Pass));
        assert_eq!(
            final_answer(Ok((held.clone(), Some(allow.clone())))),
            Some(Answer::Allow)
        );
        // An agent down, too old, stopping or confused: no answer, so nothing printed.
        assert_eq!(final_answer(Err(anyhow::anyhow!("agent down"))), None);
        assert_eq!(final_answer(Ok((held.clone(), None))), None);
        assert_eq!(
            final_answer(Ok((Response::Error("unknown request".into()), None))),
            None
        );
        assert_eq!(final_answer(Ok((Response::Ok, None))), None);
        assert_eq!(final_answer(Ok((allow, Some(held)))), None);
    }
}
