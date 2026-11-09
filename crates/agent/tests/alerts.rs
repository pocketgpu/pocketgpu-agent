//! Job alerts end to end: a phone hands over its push ticket, a job ends, and the agent's
//! signed alert reaches a fake push server.

#![cfg(unix)]

mod harness;

use std::time::Duration;

use harness::Harness;
use harness::push::fake_push_server;
use pocketgpu::jobs::runner::{self, RunOptions};
use pocketgpu_proto::msg::control::{Request, Response};
use pocketgpu_proto::push::AlertKind;
use pocketgpu_proto::{ErrorCode, Permission};

#[tokio::test]
async fn a_crashed_job_sends_a_signed_alert() {
    let h = Harness::start().await;
    let (url, mut alerts) = fake_push_server().await;
    h.agent.config.write().unwrap().push_url = url;
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    tokio::spawn(pocketgpu::alerts::sender::run(h.agent.clone()));
    while !h.agent.paths.socket().exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The phone gives the machine its ticket.
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    let ticket = "0123456789abcdef0123456789abcdef".to_string();
    let reply = machine
        .control(Request::SetPushTicket {
            ticket: ticket.clone(),
        })
        .await
        .unwrap();
    assert_eq!(reply, Response::Ok);
    match machine
        .control(Request::SetPushTicket {
            ticket: "not-a-ticket".into(),
        })
        .await
    {
        Err(pocketgpu_client::ClientError::Remote(e)) => assert_eq!(e.code, ErrorCode::Invalid),
        other => panic!("expected Invalid, got {other:?}"),
    }

    let opts = RunOptions {
        name: Some("train.py".into()),
        no_alert: false,
        command: vec!["sh".into(), "-c".into(), "exit 2".into()],
    };
    let _ = runner::run(&h.agent.paths, false, opts).await;

    let (valid, alert) = tokio::time::timeout(Duration::from_secs(10), alerts.recv())
        .await
        .expect("an alert within 10 seconds")
        .unwrap();
    assert!(valid, "the alert must be signed with the agent's key");
    assert_eq!(alert.kind, AlertKind::JobEnd);
    assert_eq!(alert.tickets, vec![ticket]);
    assert_eq!(
        alert.title,
        format!("train.py crashed · {}", h.agent.name())
    );
    assert!(alert.body.contains("Exit 2"), "{}", alert.body);
    let link = alert.link.unwrap();
    assert!(link.starts_with(&format!(
        "pocketgpu://machine/{}/job/",
        h.agent.identity.agent_id()
    )));

    // A job run with --no-alert sends nothing.
    let quiet = RunOptions {
        name: None,
        no_alert: true,
        command: vec!["false".into()],
    };
    let _ = runner::run(&h.agent.paths, false, quiet).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), alerts.recv())
            .await
            .is_err()
    );
}
