//! F32: AI assistant alerts end to end. Notes and hook alerts reach the owner's phones only,
//! as their own kind; an older push server gets them as job alerts; the switch holds them;
//! older apps get the alert list without the new rule; and `pgpu hook` stays silent.

#![cfg(unix)]

use crate::harness;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use harness::Harness;
use harness::push::{fake_push_server, old_push_server};
use pocketgpu::alerts::sender::send_note;
use pocketgpu::store::devices;
use pocketgpu_proto::msg::control::{self, AlertRule, Request, Response};
use pocketgpu_proto::push::{AlertKind, AlertRequest};
use pocketgpu_proto::{Permission, Platform};
use tokio::sync::mpsc::Receiver;

const OWNER: &str = "0123456789abcdef0123456789abcdef";
const GUEST: &str = "fedcba9876543210fedcba9876543210";
const IDLE: &str = include_str!("../fixtures/hooks/claude_notification_idle.json");

/// An owner's phone and a guest's (F44), each with a push ticket.
fn owner_and_guest(h: &Harness) {
    let db = h.agent.db();
    let add = |id: &str, ticket: &str| {
        devices::upsert(
            &db,
            id,
            id,
            Platform::Android,
            Permission::Control,
            Some(ticket),
        )
        .unwrap()
    };
    add("owner-phone", OWNER);
    add("guest-phone", GUEST);
    let ends = pocketgpu::store::db::now_ms() + 3_600_000;
    devices::set_expiry(&db, "guest-phone", Some(ends)).unwrap();
}

async fn next(alerts: &mut Receiver<(bool, AlertRequest)>) -> AlertRequest {
    let (valid, alert) = tokio::time::timeout(Duration::from_secs(10), alerts.recv())
        .await
        .expect("an alert within 10 seconds")
        .unwrap();
    assert!(valid, "signed with the agent's key");
    alert
}

async fn nothing_more(alerts: &mut Receiver<(bool, AlertRequest)>) {
    let more = tokio::time::timeout(Duration::from_millis(500), alerts.recv()).await;
    assert!(more.is_err(), "no other push: {more:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_reach_the_owner_only_as_assistant_alerts() {
    let h = Harness::start().await;
    let (url, mut alerts) = fake_push_server().await;
    h.agent.config.write().unwrap().push_url = url;
    owner_and_guest(&h);
    let sent = send_note(&h.agent, "Fine-tune finished", "Loss 0.82").await;
    assert_eq!(sent.unwrap(), (1, 0, false));
    let alert = next(&mut alerts).await;
    assert_eq!(alert.kind, AlertKind::Assistant);
    assert_eq!(alert.tickets, vec![OWNER.to_string()], "never the guest");
    assert_eq!(alert.title, "Fine-tune finished");
    assert_eq!(
        alert.link,
        Some(format!(
            "pocketgpu://machine/{}",
            h.agent.identity.agent_id()
        ))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_push_server_gets_them_as_job_alerts() {
    let h = Harness::start().await;
    let (url, mut alerts) = old_push_server().await;
    h.agent.config.write().unwrap().push_url = url;
    owner_and_guest(&h);
    let sent = send_note(&h.agent, "Done", "Tests pass").await;
    assert_eq!(sent.unwrap(), (1, 0, false));
    let alert = next(&mut alerts).await;
    assert_eq!(alert.kind, AlertKind::JobEnd);
    assert_eq!(alert.tickets, vec![OWNER.to_string()]);
    assert_eq!(alert.title, "Done");
    nothing_more(&mut alerts).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_switch_holds_them_and_leaves_job_alerts_on() {
    let h = Harness::start().await;
    let (url, mut alerts) = fake_push_server().await;
    h.agent.config.write().unwrap().push_url = url;
    owner_and_guest(&h);
    let off = AlertRule {
        kind: control::AlertKind::Assistant,
        enabled: false,
        threshold: None,
    };
    pocketgpu::alerts::settings::set(&h.agent, &[off]).unwrap();
    let cfg = h.agent.config.read().unwrap().alerts.clone();
    assert!(!cfg.assistant && cfg.job_end);
    let sent = send_note(&h.agent, "Done", "x").await.unwrap();
    assert_eq!(sent, (0, 0, true));
    nothing_more(&mut alerts).await;
}

async fn rules(m: &pocketgpu_client::Machine) -> Vec<AlertRule> {
    match m.control(Request::GetAlertSettings).await.unwrap() {
        Response::AlertSettings(r) => r,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_apps_that_know_the_kind_get_the_assistant_rule() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::Control)).await;
    let m = h.connect(&phone).await.unwrap();
    let has = |r: &[AlertRule]| r.iter().any(|r| r.kind == control::AlertKind::Assistant);
    assert!(
        !has(&rules(&m).await),
        "an app that hasn't said its version"
    );
    let hello = |proto| Request::Hello {
        app_version: "0.1.0".into(),
        proto,
    };
    m.control(hello(1)).await.unwrap();
    assert!(!has(&rules(&m).await), "an app from before the kind");
    m.control(hello(pocketgpu::alerts::settings::ASSISTANT_PROTO))
        .await
        .unwrap();
    assert!(has(&rules(&m).await));
    let off = Request::SetAlertSettings {
        rules: vec![AlertRule {
            kind: control::AlertKind::Assistant,
            enabled: false,
            threshold: None,
        }],
    };
    let Response::AlertSettings(after) = m.control(off).await.unwrap() else {
        panic!("expected AlertSettings");
    };
    let rule = after
        .iter()
        .find(|r| r.kind == control::AlertKind::Assistant)
        .unwrap();
    assert!(!rule.enabled);
    let saved = pocketgpu::store::config::Config::load(&h.agent.paths.config_file()).unwrap();
    assert!(!saved.alerts.assistant && saved.alerts.job_end);
}

#[tokio::test(flavor = "multi_thread")]
async fn guests_open_apps_never_hear_notes() {
    let h = Harness::start().await;
    let owner = h.phone(Some(Permission::Control)).await;
    let guest = h.phone(Some(Permission::Control)).await;
    let ends = pocketgpu::store::db::now_ms() + 3_600_000;
    let guest_id = guest.key.public().to_string();
    devices::set_expiry(&h.agent.db(), &guest_id, Some(ends)).unwrap();
    let (mo, mg) = (
        h.connect(&owner).await.unwrap(),
        h.connect(&guest).await.unwrap(),
    );
    let mut owner_events = mo.events().await.unwrap();
    let mut guest_events = mg.events().await.unwrap();
    while h.agent.events.receiver_count() < 2 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Neither phone has a push ticket, so only the open apps hear these.
    send_note(&h.agent, "Note for you", "x").await.unwrap();
    let job = pocketgpu::alerts::channels::Alert {
        title: "train.py finished".into(),
        body: "b".into(),
        kind: "job_end",
        machine: "m".into(),
        job: Some(1),
    };
    pocketgpu::alerts::channels::send_all(&h.agent, &job).await;
    let wait = Duration::from_secs(5);
    let first = |a: Option<pocketgpu_proto::msg::events::Alert>| a.unwrap().title;
    let o = tokio::time::timeout(wait, owner_events.next())
        .await
        .unwrap();
    assert_eq!(first(o.unwrap()), "Note for you");
    let g = tokio::time::timeout(wait, guest_events.next())
        .await
        .unwrap();
    assert_eq!(
        first(g.unwrap()),
        "train.py finished",
        "the guest skips the note"
    );
}

/// Runs the built `pocketgpu hook ...` with this input and data folder.
fn hook(data: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pocketgpu"))
        .args(args)
        .env("PGPU_DATA_DIR", data)
        .env("PGPU_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A hook that stops reading early may close its end; that's its right.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

fn assert_quiet(out: &Output, what: &str) {
    assert!(out.status.success(), "{what}: {:?}", out.status);
    assert!(out.stdout.is_empty(), "{what}: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "{what}: {:?}", out.stderr);
}

#[test]
fn the_hook_is_quiet_and_succeeds_whatever_happens() {
    // No agent listens here.
    let dir = tempfile::tempdir().unwrap();
    let big = vec![b'{'; 300 * 1024];
    let cases: [(&[&str], &[u8]); 7] = [
        (&["hook", "claude"], IDLE.as_bytes()),
        (&["hook", "claude"], b"not json at all"),
        (&["hook", "claude"], b""),
        (&["hook"], IDLE.as_bytes()),
        (&["hook", "claude", "--help"], b""),
        (
            &[
                "--json",
                "hook",
                "codex",
                r#"{"type":"agent-turn-complete"}"#,
            ],
            b"",
        ),
        (&["hook", "cursor"], &big),
    ];
    for (args, stdin) in cases {
        let started = std::time::Instant::now();
        let out = hook(dir.path(), args, stdin);
        assert_quiet(&out, &format!("{args:?}"));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{args:?} took too long"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hook_reaches_the_agent_and_the_owners_phone() {
    let h = Harness::start().await;
    let (url, mut alerts) = fake_push_server().await;
    h.agent.config.write().unwrap().push_url = url;
    owner_and_guest(&h);
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    while !h.agent.paths.socket().exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let data = h.agent.paths.data_dir.clone();
    let run = {
        let data = data.clone();
        move || hook(&data, &["hook", "claude"], IDLE.as_bytes())
    };
    let out = tokio::task::spawn_blocking(run.clone()).await.unwrap();
    assert_quiet(&out, "idle hook");
    let alert = next(&mut alerts).await;
    assert_eq!(alert.kind, AlertKind::Assistant);
    assert_eq!(alert.tickets, vec![OWNER.to_string()]);
    // Built the same way the agent builds it: a long machine name (CI runners have them) is
    // cut to the title's limit.
    let title = pocketgpu_proto::push::clip(
        &format!("Claude Code needs you · {}", h.agent.name()),
        pocketgpu_proto::push::TITLE_MAX,
    );
    assert_eq!(alert.title, title);
    assert!(alert.title.starts_with("Claude Code needs you · "));
    assert_eq!(alert.body, "Waiting for your answer at the computer");
    for secret in ["waiting for your input", "/Users", "abc123"] {
        assert!(!alert.title.contains(secret) && !alert.body.contains(secret));
    }
    // A second reminder for the same session so soon is dropped.
    let out = tokio::task::spawn_blocking(run).await.unwrap();
    assert_quiet(&out, "second idle hook");
    nothing_more(&mut alerts).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_are_counted_for_every_session_together() {
    use pocketgpu_proto::local::{Request as Local, Response as Reply};
    let h = Harness::start().await;
    let (url, mut alerts) = fake_push_server().await;
    h.agent.config.write().unwrap().push_url = url;
    owner_and_guest(&h);
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    while !h.agent.paths.socket().exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let note = |n: usize| Local::Notify {
        title: format!("Note {n}"),
        body: "Done".into(),
    };
    // Each call is a connection of its own, as each assistant session's `pgpu mcp` is.
    let limit = pocketgpu::assistants::limits::NOTES_PER_HOUR;
    for n in 0..limit {
        let reply = pocketgpu::ipc::client::call(&h.agent.paths, note(n)).await;
        assert!(
            matches!(reply, Ok(Reply::Notified { phones: 1, .. })),
            "{n}: {reply:?}"
        );
        next(&mut alerts).await;
    }
    let reply = pocketgpu::ipc::client::call(&h.agent.paths, note(limit)).await;
    let Ok(Reply::Error(e)) = reply else {
        panic!("the note after the limit: {reply:?}");
    };
    assert!(e.contains("at most 10 notes an hour"), "{e}");
    nothing_more(&mut alerts).await;
}
