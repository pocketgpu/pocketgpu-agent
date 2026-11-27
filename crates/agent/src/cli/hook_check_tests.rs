//! `pgpu hook --check` ("Ask me first", F32): what goes to the agent for Claude Code's and
//! Cursor's input (their docs' examples, in `tests/fixtures/hooks`), and what is printed for
//! each answer, each `permission_mode`, and an agent that can't be reached.

use super::*;
use crate::assistants::answers::{COULDNT_ASK, NO_ANSWER};
use crate::store::paths::Paths;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/hooks/", $name, ".json"))
    };
}

const MODES: [&str; 6] = [
    "default",
    "plan",
    "auto",
    "acceptEdits",
    "dontAsk",
    "bypassPermissions",
];

fn printed(assistant: &str, mode: &str, answer: Answer) -> Option<Value> {
    output(assistant, mode, &answer).map(|line| {
        assert!(!line.contains('\n'), "one line: {line}");
        serde_json::from_str(&line).unwrap()
    })
}

fn claude(decision: &str, reason: &str) -> Option<Value> {
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    }))
}

fn fields(req: &Request) -> (&str, &str, &str, &str, bool, &str) {
    match req {
        Request::AssistantCheck {
            assistant,
            session,
            tool,
            input,
            oversize,
            folder,
            ..
        } => (assistant, session, tool, input, *oversize, folder),
        other => panic!("{other:?}"),
    }
}

#[test]
fn claude_codes_pre_tool_use_is_sent_whole_with_its_mode() {
    let c = request("claude", fixture!("claude_pre_tool_use"), false).unwrap();
    assert_eq!(c.permission_mode, "auto");
    let (assistant, session, tool, input, oversize, folder) = fields(&c.req);
    assert_eq!(
        (assistant, session, tool, oversize, folder),
        ("claude", "abc123", "Bash", false, "/Users/.../my-project")
    );
    let input: Value = serde_json::from_str(input).unwrap();
    assert_eq!(input["command"], "git push origin main");
    // Another event isn't ours to answer.
    assert_eq!(
        request("claude", fixture!("claude_permission_request"), false),
        None
    );
    assert_eq!(
        request("codex", fixture!("claude_pre_tool_use"), false),
        None
    );
}

#[test]
fn input_too_big_or_broken_goes_as_oversize() {
    let cut = &fixture!("claude_pre_tool_use")[..200];
    for (text, over) in [
        (fixture!("claude_pre_tool_use"), true),
        (cut, false),
        ("not json", false),
        ("", true),
    ] {
        let c = request("claude", text, over).unwrap();
        let (_, _, tool, input, oversize, _) = fields(&c.req);
        assert!(oversize, "{text:?}");
        assert_eq!((tool, input), ("", ""), "nothing of the call is sent");
    }
    // The mode still counts when it can be read before the cut.
    let c = request("claude", cut, true).unwrap();
    assert_eq!(c.permission_mode, "auto");
    let c = request("cursor", "{\"command\": \"rm", true).unwrap();
    assert!(fields(&c.req).4);
}

#[test]
fn cursors_shell_and_mcp_calls() {
    let shell = request("cursor", fixture!("cursor_before_shell"), false).unwrap();
    let (assistant, session, tool, input, oversize, folder) = fields(&shell.req);
    assert_eq!(
        (assistant, session, tool, oversize, folder),
        (
            "cursor",
            "c0ffee00-aaaa-bbbb-cccc-1234567890ab",
            "shell",
            false,
            "/Users/ana/proj"
        )
    );
    assert_eq!(input, r#"{"command":"rm -rf dist"}"#);
    assert_eq!(
        crate::assistants::kinds::classify(tool, input),
        pocketgpu_proto::assistants::Kind::Delete
    );
    let mcp = request("cursor", fixture!("cursor_before_mcp"), false).unwrap();
    let (_, _, tool, input, _, folder) = fields(&mcp.req);
    assert_eq!(tool, "deploy");
    assert!(input.contains("terraform apply"));
    assert_eq!(folder, "/Users/ana/proj");
    // Week 1's events aren't answered by `--check`.
    assert_eq!(request("cursor", fixture!("cursor_stop"), false), None);
}

#[test]
fn the_phones_answer_for_claude_code() {
    for mode in MODES {
        assert_eq!(
            printed("claude", mode, Answer::Allow),
            claude("allow", "Allowed on your phone")
        );
        assert_eq!(
            printed(
                "claude",
                mode,
                Answer::Deny {
                    message: "Denied on your phone".into()
                }
            ),
            claude("deny", "Denied on your phone")
        );
        assert_eq!(printed("claude", mode, Answer::Pass), None);
        // The clock always denies.
        assert_eq!(
            printed(
                "claude",
                mode,
                Answer::NoDecision {
                    why: Ended::TimedOut
                }
            ),
            claude("deny", NO_ANSWER)
        );
    }
}

#[test]
fn no_decision_asks_at_the_desk_only_where_a_prompt_shows() {
    for mode in MODES {
        for why in [
            Ended::NoPhone,
            Ended::AnsweredAtComputer,
            Ended::NoLongerNeeded,
        ] {
            let want = if matches!(mode, "default" | "plan") {
                claude("ask", COULDNT_ASK)
            } else {
                claude("deny", COULDNT_ASK)
            };
            assert_eq!(
                printed("claude", mode, Answer::NoDecision { why }),
                want,
                "{mode}"
            );
        }
    }
    // Missing mode (an old Claude Code): the safe side.
    assert_eq!(
        printed(
            "claude",
            "",
            Answer::NoDecision {
                why: Ended::NoPhone
            }
        ),
        claude("deny", COULDNT_ASK)
    );
}

#[test]
fn cursors_answers() {
    assert_eq!(
        printed("cursor", "", Answer::Allow),
        Some(json!({ "permission": "allow" }))
    );
    let deny =
        |m: &str| Some(json!({ "permission": "deny", "user_message": m, "agent_message": m }));
    assert_eq!(
        printed(
            "cursor",
            "",
            Answer::Deny {
                message: "Denied on your phone".into()
            }
        ),
        deny("Denied on your phone")
    );
    assert_eq!(
        printed(
            "cursor",
            "",
            Answer::NoDecision {
                why: Ended::TimedOut
            }
        ),
        deny(NO_ANSWER)
    );
    assert_eq!(
        printed(
            "cursor",
            "",
            Answer::NoDecision {
                why: Ended::NoPhone
            }
        ),
        Some(
            json!({ "permission": "ask", "user_message": COULDNT_ASK, "agent_message": COULDNT_ASK })
        )
    );
    assert_eq!(printed("cursor", "", Answer::Pass), None);
}

#[test]
fn the_agents_answer_or_the_guard() {
    let pass = Response::AssistantAnswer(Answer::Pass);
    let allow = Response::AssistantAnswer(Answer::Allow);
    let held = Response::AssistantHeld { id: 7 };
    let never = || -> bool { panic!("the guard is read only when the agent can't answer") };
    assert_eq!(
        decide("claude", "auto", Ok((pass, None)), never),
        Some(Answer::Pass)
    );
    assert_eq!(
        decide("claude", "auto", Ok((held.clone(), Some(allow))), never),
        Some(Answer::Allow)
    );
    let deny = Some(Answer::Deny {
        message: COULDNT_ASK.into(),
    });
    let ask = Some(Answer::NoDecision {
        why: Ended::NoPhone,
    });
    // Held, then the answer was lost: never let through, whatever the guard says.
    assert_eq!(
        decide("claude", "auto", Err(HeldLost.into()), || false),
        deny
    );
    assert_eq!(
        decide("claude", "default", Err(HeldLost.into()), || false),
        ask
    );
    assert_eq!(
        decide("claude", "auto", Ok((held.clone(), None)), || false),
        deny
    );
    // The agent down, too old or confused: the guard decides.
    for replies in [
        || Err(anyhow::anyhow!("agent down")),
        || Ok((Response::Error("unknown request".into()), None)),
        || Ok((Response::Ok, None)),
    ] {
        assert_eq!(decide("claude", "auto", replies(), || true), deny);
        assert_eq!(decide("claude", "plan", replies(), || true), ask);
        assert_eq!(decide("cursor", "", replies(), || true), ask);
        assert_eq!(decide("claude", "auto", replies(), || false), None);
    }
}

#[test]
fn with_the_agent_down_the_guard_file_decides() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path().to_path_buf());
    let down = || Err(anyhow::anyhow!("agent down"));
    let armed = || crate::assistants::guard::armed(&paths);
    assert_eq!(decide("claude", "bypassPermissions", down(), armed), None);
    crate::assistants::guard::write(&paths, true).unwrap();
    let answer = decide("claude", "bypassPermissions", down(), armed).unwrap();
    assert_eq!(
        output("claude", "bypassPermissions", &answer).map(|l| serde_json::from_str(&l).unwrap()),
        claude("deny", COULDNT_ASK)
    );
}
