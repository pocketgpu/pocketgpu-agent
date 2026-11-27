//! F32 approvals end to end: the built `pocketgpu hook claude --ask` as Claude Code runs it,
//! over the real local socket, and a phone over the real control protocol. Allow, Deny, the
//! desk answering first, the clock, a request from before a restart, and who may answer.

#![cfg(unix)]

use crate::harness;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use harness::push::fake_push_server;
use harness::{Harness, Phone};
use pocketgpu::assistants::hold;
use pocketgpu::daemon::state::Agent;
use pocketgpu::store::config::AssistantConfig;
use pocketgpu::store::devices;
use pocketgpu_client::{ClientError, Machine};
use pocketgpu_proto::assistants::{ApprovalState, ApprovalView, AskWhy, Ended, Kind};
use pocketgpu_proto::msg::control::{Request, Response};
use pocketgpu_proto::push::{AlertKind, AlertRequest};
use pocketgpu_proto::{ErrorCode, Permission};
use serde_json::{Value, json};
use tokio::sync::mpsc::Receiver;
use tokio::task::JoinHandle;

pub(super) const TICKET: &str = "0123456789abcdef0123456789abcdef";
pub(super) const WAIT: Duration = Duration::from_secs(10);

/// What Claude Code hands its `PermissionRequest` hook.
fn permission_request(session: &str, command: &str) -> String {
    json!({
        "session_id": session,
        "transcript_path": "/home/me/.claude/projects/p/t.jsonl",
        "cwd": "/home/me/secret-project",
        "permission_mode": "default",
        "hook_event_name": "PermissionRequest",
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "Clean up" },
    })
    .to_string()
}

/// What it hands the async alert hook when the call ran.
fn post_tool_use(session: &str, command: &str) -> String {
    json!({
        "session_id": session,
        "cwd": "/home/me/secret-project",
        "permission_mode": "default",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "Clean up" },
        "tool_response": { "stdout": "", "stderr": "" },
        "tool_use_id": "toolu_01",
    })
    .to_string()
}

/// Runs the built `pocketgpu` with these arguments and input, as an assistant would.
pub(super) fn run_hook(data: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pocketgpu"))
        .args(args)
        .env("PGPU_DATA_DIR", data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    child.wait_with_output().unwrap()
}

/// `pocketgpu hook claude --ask`, waiting in the background like Claude Code.
fn ask(agent: &Agent, session: &str, command: &str) -> JoinHandle<Output> {
    let data = agent.paths.data_dir.clone();
    let input = permission_request(session, command);
    tokio::task::spawn_blocking(move || run_hook(&data, &["hook", "claude", "--ask"], &input))
}

/// What the hook printed, as JSON; `None` for nothing. It always succeeds and is quiet.
pub(super) async fn printed(hook: JoinHandle<Output>) -> Option<Value> {
    let out = tokio::time::timeout(WAIT, hook)
        .await
        .expect("the hook ends in time")
        .unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    assert!(
        out.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    (!text.trim().is_empty()).then(|| serde_json::from_str(text.trim()).unwrap())
}

fn decision(v: &Value) -> &Value {
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"],
        "PermissionRequest"
    );
    &v["hookSpecificOutput"]["decision"]
}

/// An agent with "Ask on my phone" on, its local socket open and a fake push server.
async fn agent_asking() -> (Harness, Receiver<(bool, AlertRequest)>) {
    let h = Harness::start().await;
    let (url, alerts) = fake_push_server().await;
    h.agent.config.write().unwrap().push_url = url;
    let cfg = AssistantConfig {
        ask_on_phone: true,
        ..Default::default()
    };
    hold::save_settings(&h.agent, cfg, "local").unwrap();
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    while !h.agent.paths.socket().exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (h, alerts)
}

/// The owner's Full control phone, with a push ticket, connected.
pub(super) async fn full_control(h: &Harness) -> (Phone, Machine) {
    let phone = h.phone(Some(Permission::Shell)).await;
    devices::set_push_ticket(&h.agent.db(), &phone.key.public().to_string(), TICKET).unwrap();
    let m = h.connect(&phone).await.unwrap();
    (phone, m)
}

pub(super) async fn next_push(alerts: &mut Receiver<(bool, AlertRequest)>) -> AlertRequest {
    let (valid, alert) = tokio::time::timeout(WAIT, alerts.recv())
        .await
        .expect("a push in time")
        .unwrap();
    assert!(valid, "signed with the agent's key");
    alert
}

pub(super) async fn waiting(m: &Machine) -> Vec<ApprovalView> {
    match m.control(Request::ListApprovals).await.unwrap() {
        Response::Approvals(list) => list
            .into_iter()
            .filter(|v| v.state == ApprovalState::Waiting)
            .collect(),
        other => panic!("{other:?}"),
    }
}

pub(super) async fn answer(
    m: &Machine,
    v: &ApprovalView,
    allow: bool,
) -> Result<ApprovalView, ClientError> {
    let req = Request::AnswerApproval {
        id: v.id,
        opened_ms: v.opened_ms,
        allow,
    };
    match m.control(req).await? {
        Response::ApprovalAnswered(view) => Ok(*view),
        other => panic!("{other:?}"),
    }
}

pub(super) fn refused(r: Result<ApprovalView, ClientError>, code: ErrorCode, says: &str) {
    match r {
        Err(ClientError::Remote(e)) => {
            assert_eq!(e.code, code, "{e:?}");
            assert!(e.message.contains(says), "{e:?}");
        }
        other => panic!("expected a refusal: {other:?}"),
    }
}

/// Waits until the phone sees exactly one waiting request (the push came first).
pub(super) async fn the_one_waiting(m: &Machine) -> ApprovalView {
    for _ in 0..100 {
        let list = waiting(m).await;
        if list.len() == 1 {
            return list.into_iter().next().unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no request waiting");
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_on_the_phone_lets_claude_code_go_on() {
    let (h, mut alerts) = agent_asking().await;
    let (_phone, m) = full_control(&h).await;
    let mut events = m.events().await.unwrap();
    while h.agent.events.receiver_count() < 1 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let hook = ask(&h.agent, "sess-allow", "rm -rf build dist .cache");

    // The push: Pro kind, the owner's Full control phone, the assistant and computer only.
    let push = next_push(&mut alerts).await;
    assert_eq!(push.kind, AlertKind::Approval);
    assert_eq!(push.tickets, vec![TICKET.to_string()]);
    assert!(
        push.title.starts_with("Claude Code asks you · "),
        "{}",
        push.title
    );
    assert_eq!(push.body, "Tap to see what it wants and answer.");
    for secret in ["rm -rf", "build", ".cache", "secret-project", "sess-allow"] {
        assert!(!push.title.contains(secret) && !push.body.contains(secret));
    }

    let v = the_one_waiting(&m).await;
    assert_eq!(
        push.link,
        Some(format!(
            "pocketgpu://machine/{}/assistants/{}",
            h.agent.identity.agent_id(),
            v.id
        ))
    );
    assert_eq!(v.why, AskWhy::AskOnPhone);
    assert_eq!(v.kind, Kind::Delete);
    assert!(v.risky);
    assert_eq!(v.detail, "rm -rf build dist .cache");
    assert_eq!(v.folder, "/home/me/secret-project");
    assert_eq!(v.session_short, "sess-all");

    // Open apps of Full control devices hear it, with the id.
    let opened = tokio::time::timeout(WAIT, events.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(opened.kind, "approval_opened");
    assert_eq!(opened.job, Some(v.id));
    assert!(!opened.title.contains("rm -rf"));

    let after = answer(&m, &v, true).await.unwrap();
    assert_eq!(after.state, ApprovalState::Allowed);
    assert_eq!(after.answered_by_name.as_deref(), Some("Test phone"));
    let out = printed(hook).await.expect("an allow");
    assert_eq!(decision(&out), &json!({ "behavior": "allow" }));

    // The first answer wins.
    refused(
        answer(&m, &v, false).await,
        ErrorCode::Busy,
        "already allowed on Test phone",
    );
    assert_eq!(h.audit_events("assistant_request"), 1);

    // A request from before a restart: a new agent run has never heard of the id.
    let (h2, _alerts2) = agent_asking().await;
    let (_p2, m2) = full_control(&h2).await;
    refused(
        answer(&m2, &v, true).await,
        ErrorCode::NotFound,
        "no longer open",
    );
    // And a wrong `opened_ms` on this one is refused too.
    let stale = ApprovalView {
        opened_ms: v.opened_ms - 1,
        ..v
    };
    refused(
        answer(&m, &stale, true).await,
        ErrorCode::NotFound,
        "no longer open",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_on_the_phone_stops_the_call() {
    let (h, mut alerts) = agent_asking().await;
    let (_phone, m) = full_control(&h).await;
    let hook = ask(&h.agent, "sess-deny", "git push --force origin main");
    next_push(&mut alerts).await;
    let v = the_one_waiting(&m).await;
    assert_eq!(v.kind, Kind::GitPush);
    let after = answer(&m, &v, false).await.unwrap();
    assert_eq!(after.state, ApprovalState::Denied);
    let out = printed(hook).await.expect("a deny");
    assert_eq!(
        decision(&out),
        &json!({ "behavior": "deny", "message": "Denied on your phone" })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_desk_answering_first_ends_the_request() {
    let (h, mut alerts) = agent_asking().await;
    let (_phone, m) = full_control(&h).await;
    let cmd = "rm -rf node_modules";
    let hook = ask(&h.agent, "sess-desk", cmd);
    next_push(&mut alerts).await;
    let v = the_one_waiting(&m).await;
    // Another call in the same session finishing doesn't end it.
    let data = h.agent.paths.data_dir.clone();
    let other = post_tool_use("sess-desk", "ls -la");
    let d = data.clone();
    tokio::task::spawn_blocking(move || run_hook(&d, &["hook", "claude"], &other))
        .await
        .unwrap();
    assert!(
        h.agent
            .assistants
            .approvals
            .get(v.id)
            .unwrap()
            .state
            .is_open()
    );
    // This call ran: answered at the computer.
    let done = post_tool_use("sess-desk", cmd);
    tokio::task::spawn_blocking(move || run_hook(&data, &["hook", "claude"], &done))
        .await
        .unwrap();
    assert_eq!(printed(hook).await, None, "the desk prompt decides");
    assert_eq!(
        h.agent.assistants.approvals.get(v.id).unwrap().state,
        ApprovalState::AnsweredAtComputer
    );
    refused(
        answer(&m, &v, true).await,
        ErrorCode::Busy,
        "already answered at the computer",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_clock_never_allows() {
    let (h, mut alerts) = agent_asking().await;
    let (_phone, m) = full_control(&h).await;
    let hook = ask(&h.agent, "sess-clock", "rm -rf build");
    next_push(&mut alerts).await;
    let v = the_one_waiting(&m).await;
    // What the 5-minute timer does when it fires.
    hold::end(&h.agent, v.id, Ended::TimedOut);
    assert_eq!(printed(hook).await, None, "the desk prompt stays");
    refused(
        answer(&m, &v, true).await,
        ErrorCode::NotFound,
        "no longer open",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_owners_full_control_phones_see_or_answer() {
    let (h, mut alerts) = agent_asking().await;
    let (_full, m) = full_control(&h).await;
    let control = h.phone(Some(Permission::Control)).await;
    let mc = h.connect(&control).await.unwrap();
    let guest = h.phone(Some(Permission::Shell)).await;
    let ends = pocketgpu::store::db::now_ms() + 3_600_000;
    devices::set_expiry(&h.agent.db(), &guest.key.public().to_string(), Some(ends)).unwrap();
    let mg = h.connect(&guest).await.unwrap();

    let hook = ask(&h.agent, "sess-who", "rm -rf build");
    let push = next_push(&mut alerts).await;
    assert_eq!(push.tickets, vec![TICKET.to_string()]);
    let v = the_one_waiting(&m).await;
    for (who, phone) in [("control", &mc), ("guest", &mg)] {
        let list = phone.control(Request::ListApprovals).await;
        assert!(
            matches!(list, Err(ClientError::Remote(ref e)) if e.code == ErrorCode::Forbidden),
            "{who}: {list:?}"
        );
        refused(answer(phone, &v, true).await, ErrorCode::Forbidden, "");
    }
    assert!(
        h.agent
            .assistants
            .approvals
            .get(v.id)
            .unwrap()
            .state
            .is_open()
    );
    // Lowered while it waits: the answer is refused.
    let full_id = h
        .agent
        .db()
        .query_row(
            "SELECT device_id FROM devices WHERE permission = 'shell' AND expires_ms IS NULL",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    devices::set_permission(&h.agent.db(), &full_id, Permission::Control).unwrap();
    refused(answer(&m, &v, true).await, ErrorCode::Forbidden, "");
    hold::end(&h.agent, v.id, Ended::TimedOut);
    assert_eq!(printed(hook).await, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_phone_reached_means_the_desk_at_once() {
    // No Full control phone with a ticket: the push reaches nobody.
    let (h, mut alerts) = agent_asking().await;
    let _control = h.phone(Some(Permission::Control)).await;
    let started = std::time::Instant::now();
    let hook = ask(&h.agent, "sess-none", "rm -rf build");
    assert_eq!(printed(hook).await, None);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(alerts.try_recv().is_err(), "no push");
    let ended = h
        .agent
        .assistants
        .approvals
        .list(pocketgpu::store::db::now_ms());
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].state, ApprovalState::NoPhone);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_switch_off_the_hook_passes_at_once() {
    let h = Harness::start().await;
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    while !h.agent.paths.socket().exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let agent: Arc<Agent> = h.agent.clone();
    assert_eq!(printed(ask(&agent, "s", "rm -rf build")).await, None);
    assert!(
        agent
            .assistants
            .approvals
            .list(pocketgpu::store::db::now_ms())
            .is_empty()
    );
}
