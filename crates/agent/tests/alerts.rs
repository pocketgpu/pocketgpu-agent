//! Job alerts end to end: a phone hands over its push ticket, a job ends, and the agent's
//! signed alert reaches a fake push server.

#![cfg(unix)]

mod harness;

use std::time::Duration;

use harness::Harness;
use iroh::{PublicKey, Signature};
use pocketgpu::jobs::runner::{self, RunOptions};
use pocketgpu_proto::msg::control::{Request, Response};
use pocketgpu_proto::push::{AlertKind, AlertRequest, signing_message};
use pocketgpu_proto::{ErrorCode, Permission};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Accepts HTTP posts, checks the agent signature, and passes the alerts on.
async fn fake_push_server() -> (String, mpsc::Receiver<(bool, AlertRequest)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head, body) = loop {
                let n = s.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(i) = text.find("\r\n\r\n") {
                    let head = text[..i].to_string();
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < i + 4 + len {
                        let n = s.read(&mut chunk).await.unwrap();
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    break (head, buf[i + 4..i + 4 + len].to_vec());
                }
            };
            let header = |name: &str| {
                head.lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(": ")?;
                        k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                    })
                    .unwrap()
            };
            let key: PublicKey = header("x-agent-id").parse().unwrap();
            let ts: u64 = header("x-timestamp").parse().unwrap();
            let sig_hex = header("x-signature");
            let sig: Vec<u8> = (0..64)
                .map(|i| u8::from_str_radix(&sig_hex[i * 2..i * 2 + 2], 16).unwrap())
                .collect();
            let sig = Signature::from_bytes(&sig.try_into().unwrap());
            let valid = key
                .verify(&signing_message("POST", "/v1/alerts", ts, &body), &sig)
                .is_ok();
            let alert: AlertRequest = serde_json::from_slice(&body).unwrap();
            let reply = r#"{"delivered":1,"unknown":0,"not_entitled":0}"#;
            let res = format!(
                "HTTP/1.1 202 Accepted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            s.write_all(res.as_bytes()).await.unwrap();
            let _ = tx.send((valid, alert)).await;
        }
    });
    (url, rx)
}

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
