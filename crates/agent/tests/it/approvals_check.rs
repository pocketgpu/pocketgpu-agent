//! F32 "Ask me first" end to end: the built `pocketgpu hook claude --check` as Claude Code's
//! `PreToolUse` runs it, over the real local socket. A held action in auto mode, the phone's
//! Allow, the clock, a hook killed while it waits, and the agent down with and without the
//! guard file. Nothing held ever runs without the phone's Allow.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::approvals::{
    TICKET, WAIT, answer, full_control, next_push, printed, run_hook, the_one_waiting,
};
use crate::harness::Harness;
use crate::harness::push::fake_push_server;
use pocketgpu::assistants::hold;
use pocketgpu::store::config::AssistantConfig;
use pocketgpu_proto::assistants::{ApprovalState, AskWhy, Ended, Kind};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

/// What Claude Code hands its `PreToolUse` hook.
fn pre_tool_use(session: &str, mode: &str, command: &str) -> String {
    json!({
        "session_id": session,
        "transcript_path": "/home/me/.claude/projects/p/t.jsonl",
        "cwd": "/home/me/secret-project",
        "permission_mode": mode,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "Clean up" },
        "tool_use_id": "toolu_01",
    })
    .to_string()
}

fn check(
    data: &Path,
    session: &str,
    mode: &str,
    command: &str,
) -> JoinHandle<std::process::Output> {
    let data = data.to_path_buf();
    let input = pre_tool_use(session, mode, command);
    tokio::task::spawn_blocking(move || run_hook(&data, &["hook", "claude", "--check"], &input))
}

fn decision(v: &Value) -> (&str, &str) {
    let out = &v["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse");
    (
        out["permissionDecision"].as_str().unwrap(),
        out["permissionDecisionReason"].as_str().unwrap(),
    )
}

/// An agent with "Ask me first" on for deleting files, its socket open, a fake push server.
async fn agent_checking() -> (
    Harness,
    tokio::sync::mpsc::Receiver<(bool, pocketgpu_proto::push::AlertRequest)>,
) {
    let h = Harness::start().await;
    let (url, alerts) = fake_push_server().await;
    h.agent.config.write().unwrap().push_url = url;
    let cfg = AssistantConfig {
        ask_first: vec!["delete".into()],
        ..Default::default()
    };
    hold::save_settings(&h.agent, cfg, "local").unwrap();
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    while !h.agent.paths.socket().exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (h, alerts)
}

#[tokio::test(flavor = "multi_thread")]
async fn held_in_auto_mode_until_the_phone_allows() {
    let (h, mut alerts) = agent_checking().await;
    let (_phone, m) = full_control(&h).await;
    let data = h.agent.paths.data_dir.clone();
    // Not a chosen kind: passes at once.
    assert_eq!(printed(check(&data, "s1", "auto", "ls -la")).await, None);

    let hook = check(&data, "s1", "auto", "rm -rf build dist .cache");
    let push = next_push(&mut alerts).await;
    assert_eq!(push.tickets, vec![TICKET.to_string()]);
    assert!(!push.body.contains("rm") && !push.title.contains("build"));
    let v = the_one_waiting(&m).await;
    assert_eq!((v.why, v.kind), (AskWhy::AskFirst, Kind::Delete));
    assert!(v.risky);
    answer(&m, &v, true).await.unwrap();
    let out = printed(hook).await.expect("an allow");
    assert_eq!(decision(&out), ("allow", "Allowed on your phone"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_clock_denies_in_every_mode() {
    let (h, mut alerts) = agent_checking().await;
    let (_phone, m) = full_control(&h).await;
    let data = h.agent.paths.data_dir.clone();
    for mode in ["default", "bypassPermissions"] {
        let hook = check(&data, mode, mode, "rm -rf build");
        next_push(&mut alerts).await;
        let v = the_one_waiting(&m).await;
        // What the 5-minute timer does when it fires.
        hold::end(&h.agent, v.id, Ended::TimedOut);
        let out = printed(hook).await.expect("a deny");
        assert_eq!(
            decision(&out),
            ("deny", "No answer on your phone in 5 minutes"),
            "{mode}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_hook_ends_the_request() {
    let (h, mut alerts) = agent_checking().await;
    let (_phone, m) = full_control(&h).await;
    let mut child = Command::new(env!("CARGO_BIN_EXE_pocketgpu"))
        .args(["hook", "claude", "--check"])
        .env("PGPU_DATA_DIR", &h.agent.paths.data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(pre_tool_use("s-kill", "auto", "rm -rf build").as_bytes())
            .unwrap();
    }
    next_push(&mut alerts).await;
    let v = the_one_waiting(&m).await;
    child.kill().unwrap();
    child.wait().unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let state = h.agent.assistants.approvals.get(v.id).unwrap().state;
        if state == ApprovalState::NoLongerNeeded {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "still {state:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Nothing is left to allow.
    assert!(answer(&m, &v, true).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_down_and_the_guard_file() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    // No guard: the switches are off, so the call goes on as without PocketGPU.
    assert_eq!(printed(check(&data, "s", "auto", "rm -rf /")).await, None);
    // Armed: denied where no prompt would show, the desk prompt where one does.
    let paths = pocketgpu::store::paths::Paths::under(data.clone());
    pocketgpu::assistants::guard::write(&paths, true).unwrap();
    let out = printed(check(&data, "s", "auto", "ls")).await.unwrap();
    assert_eq!(
        decision(&out),
        ("deny", "PocketGPU couldn't ask your phone")
    );
    let out = printed(check(&data, "s", "default", "ls")).await.unwrap();
    assert_eq!(decision(&out).0, "ask");
}

#[tokio::test(flavor = "multi_thread")]
async fn input_too_big_is_never_passed_while_armed() {
    let (h, _alerts) = agent_checking().await;
    let data = h.agent.paths.data_dir.clone();
    let big = "x".repeat(pocketgpu_proto::assistants::INPUT_MAX + 10);
    let hook = check(&data, "s-big", "acceptEdits", &format!("echo {big}"));
    let out = printed(hook).await.expect("a deny");
    assert_eq!(
        decision(&out),
        ("deny", "PocketGPU couldn't ask your phone")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_control_phone_turns_ask_me_first_on() {
    use pocketgpu_proto::Permission;
    use pocketgpu_proto::assistants::AssistantSettings;
    use pocketgpu_proto::msg::control::{Request, Response};
    let h = Harness::start().await;
    let (_phone, m) = full_control(&h).await;
    let want = AssistantSettings {
        ask_first: vec![Kind::Delete, Kind::GitPush],
        own_words: vec!["terraform apply".into()],
        ..Default::default()
    };
    match m
        .control(Request::SetAssistantSettings(want.clone()))
        .await
        .unwrap()
    {
        Response::Assistants { settings, .. } => assert_eq!(settings, Some(want.clone())),
        other => panic!("{other:?}"),
    }
    assert!(pocketgpu::assistants::guard::armed(&h.agent.paths));
    assert_eq!(h.audit_events("assistant_settings"), 1);
    let saved = h.agent.config.read().unwrap().assistants.clone();
    assert_eq!(saved.ask_first, ["delete", "git-push"]);

    // A Control phone can't change them.
    let control = h.phone(Some(Permission::Control)).await;
    let mc = h.connect(&control).await.unwrap();
    let off = AssistantSettings::default();
    assert!(
        mc.control(Request::SetAssistantSettings(off.clone()))
            .await
            .is_err()
    );
    assert!(pocketgpu::assistants::guard::armed(&h.agent.paths));
    // Settings that don't check out are refused whole.
    let bad = AssistantSettings {
        own_words: vec!["x".repeat(500)],
        ..off.clone()
    };
    assert!(m.control(Request::SetAssistantSettings(bad)).await.is_err());
    // Off again: the guard goes.
    m.control(Request::SetAssistantSettings(off)).await.unwrap();
    assert!(!pocketgpu::assistants::guard::armed(&h.agent.paths));
}
