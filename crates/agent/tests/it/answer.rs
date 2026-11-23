//! F17/F18: chat answers and model downloads the agent runs itself, so they go on when the
//! phone goes away (`ToAgent::Answer`, `ToAgent::Pulls`), against a slow fake Ollama.

use std::time::Duration;

use crate::harness;

use harness::Harness;
use pocketgpu_client::{ClientError, OllamaReply};
use pocketgpu_proto::tunnel::{AnswerAction, PullAction};
use pocketgpu_proto::{ErrorCode, Permission};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PIECES: usize = 5;

/// An Ollama that writes `/api/chat` and `/api/pull` a line every 100 ms.
async fn slow_ollama() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let pull = String::from_utf8_lossy(&buf[..n]).starts_with("POST /api/pull");
                let head = "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\nconnection: close\r\n\r\n";
                let _ = s.write_all(head.as_bytes()).await;
                for i in 1..=PIECES {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let line = if pull {
                        format!(
                            "{{\"status\":\"pulling a\",\"digest\":\"a\",\"completed\":{i},\"total\":{PIECES}}}\n"
                        )
                    } else {
                        format!("{{\"message\":{{\"content\":\"w{i} \"}},\"done\":false}}\n")
                    };
                    if s.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                }
                let last = if pull {
                    "{\"status\":\"success\"}\n"
                } else {
                    "{\"message\":{\"content\":\"\"},\"done\":true}\n"
                };
                let _ = s.write_all(last.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}

async fn read_all(mut reply: OllamaReply) -> String {
    let mut out = Vec::new();
    while let Some(part) = reply.chunk().await.unwrap() {
        out.extend(part);
    }
    String::from_utf8(out).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_finishes_while_the_phone_is_away() {
    let h = Harness::start().await;
    h.agent.config.write().unwrap().ollama_addr = slow_ollama().await;
    let phone = h.phone(Some(Permission::Control)).await;
    let m = h.connect(&phone).await.unwrap();
    let body = br#"{"model":"qwen3:14b","messages":[],"stream":true}"#.to_vec();

    // The first piece, then the phone goes away.
    let mut reply = m
        .answer(AnswerAction::Start {
            id: 7,
            port: None,
            body,
        })
        .await
        .unwrap();
    assert_eq!(reply.status, 200);
    let first = reply.chunk().await.unwrap().unwrap();
    drop(reply);
    tokio::time::sleep(Duration::from_millis(800)).await;

    // The rest is there when it comes back, from where it was.
    let from = first.len() as u64;
    let rest = read_all(
        m.answer(AnswerAction::Resume { id: 7, from })
            .await
            .unwrap(),
    )
    .await;
    let whole = format!("{}{rest}", String::from_utf8(first).unwrap());
    for i in 1..=PIECES {
        assert!(whole.contains(&format!("w{i} ")), "{whole}");
    }
    assert!(whole.trim_end().ends_with("\"done\":true}"), "{whole}");

    // It is this phone's own.
    let other = h.phone(Some(Permission::Control)).await;
    let o = h.connect(&other).await.unwrap();
    match o.answer(AnswerAction::Resume { id: 7, from: 0 }).await {
        Err(ClientError::Remote(e)) => assert_eq!(e.code, ErrorCode::NotFound),
        Ok(_) => panic!("another phone read the answer"),
        Err(e) => panic!("expected a refusal, got {e}"),
    }

    // Stopped, it is gone.
    read_all(m.answer(AnswerAction::Stop { id: 7 }).await.unwrap()).await;
    assert!(matches!(
        m.answer(AnswerAction::Resume { id: 7, from: 0 }).await,
        Err(ClientError::Remote(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_cannot_start_an_answer() {
    let h = Harness::start().await;
    h.agent.config.write().unwrap().ollama_addr = slow_ollama().await;
    let viewer = h.phone(Some(Permission::View)).await;
    let v = h.connect(&viewer).await.unwrap();
    let start = AnswerAction::Start {
        id: 1,
        port: None,
        body: vec![],
    };
    match v.answer(start).await {
        Err(ClientError::Remote(e)) => assert_eq!(e.code, ErrorCode::Forbidden),
        Ok(_) => panic!("a viewer started a chat"),
        Err(e) => panic!("expected a refusal, got {e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_goes_on_without_the_phone() {
    let h = Harness::start().await;
    h.agent.config.write().unwrap().ollama_addr = slow_ollama().await;
    let owner = h.phone(Some(Permission::Shell)).await;
    let o = h.connect(&owner).await.unwrap();
    let start = PullAction::Start {
        model: "gemma3".into(),
    };
    assert_eq!(o.pulls(start).await.unwrap()[0].model, "gemma3:latest");
    drop(o);

    // Another phone that may only look sees it finish.
    let viewer = h.phone(Some(Permission::View)).await;
    let v = h.connect(&viewer).await.unwrap();
    let mut done = None;
    for _ in 0..40 {
        let list = v.pulls(PullAction::List).await.unwrap();
        if list[0].done {
            done = Some(list[0].clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let p = done.expect("the download finished");
    assert_eq!(p.error, None);
    assert_eq!((p.completed, p.total), (PIECES as u64, PIECES as u64));
    assert_eq!(h.audit_events("model_pull"), 1);

    // It may clear the finished one, not stop a running one.
    let again = PullAction::Start {
        model: "gemma3".into(),
    };
    match v.pulls(again).await {
        Err(ClientError::Remote(e)) => assert_eq!(e.code, ErrorCode::Forbidden),
        r => panic!("a viewer started a download: {:?}", r.map(|_| ())),
    }
    let stop = PullAction::Stop {
        model: "gemma3".into(),
    };
    assert!(v.pulls(stop).await.unwrap().is_empty());
}
