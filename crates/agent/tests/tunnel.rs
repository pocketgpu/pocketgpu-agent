//! F17/F18: requests to Ollama through `pgpu/tunnel/1`, against a fake Ollama.

mod harness;

use harness::Harness;
use pocketgpu_client::ClientError;
use pocketgpu_proto::tunnel::Method;
use pocketgpu_proto::{ErrorCode, Permission};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A tiny Ollama: `/api/tags` answers JSON, `/api/pull` streams three progress lines.
async fn fake_ollama() -> String {
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
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.starts_with("GET /api/tags") {
                    r#"{"models":[{"name":"qwen3:14b","size":9000000000}]}"#.to_string()
                } else if req.starts_with("POST /api/pull") {
                    (1..=3)
                        .map(|i| {
                            format!("{{\"status\":\"pulling\",\"completed\":{i},\"total\":3}}\n")
                        })
                        .collect()
                } else {
                    r#"{"error":"not found"}"#.to_string()
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(body.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}

fn code<T>(r: Result<T, ClientError>) -> ErrorCode {
    match r {
        Err(ClientError::Remote(e)) => e.code,
        Ok(_) => panic!("expected a refusal"),
        Err(e) => panic!("expected a refusal, got {e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn phones_reach_ollama_within_their_permission() {
    let h = Harness::start().await;
    h.agent.config.write().unwrap().ollama_addr = fake_ollama().await;

    let viewer = h.phone(Some(Permission::View)).await;
    let v = h.connect(&viewer).await.unwrap();
    let reply = v.ollama(Method::Get, "/api/tags", vec![]).await.unwrap();
    assert_eq!(reply.status, 200);
    let body = String::from_utf8(reply.body().await.unwrap()).unwrap();
    assert!(body.contains("qwen3:14b"), "{body}");

    let chat = br#"{"model":"qwen3:14b","messages":[]}"#.to_vec();
    assert_eq!(
        code(v.ollama(Method::Post, "/api/chat", chat).await),
        ErrorCode::Forbidden
    );

    let controller = h.phone(Some(Permission::Control)).await;
    let c = h.connect(&controller).await.unwrap();
    let pull = br#"{"model":"gemma3:12b"}"#.to_vec();
    assert_eq!(
        code(c.ollama(Method::Post, "/api/pull", pull.clone()).await),
        ErrorCode::Forbidden
    );
    assert_eq!(
        code(c.ollama(Method::Post, "/api/create", vec![]).await),
        ErrorCode::Invalid
    );

    let owner = h.phone(Some(Permission::Shell)).await;
    let o = h.connect(&owner).await.unwrap();
    let mut reply = o.ollama(Method::Post, "/api/pull", pull).await.unwrap();
    let mut lines = String::new();
    while let Some(part) = reply.chunk().await.unwrap() {
        lines.push_str(&String::from_utf8_lossy(&part));
    }
    assert_eq!(lines.lines().count(), 3, "{lines}");
    assert_eq!(h.audit_events("model_pull"), 1);
    assert_eq!(h.audit_events("refused"), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_machine_without_ollama_says_so() {
    let h = Harness::start().await;
    h.agent.config.write().unwrap().ollama_addr = "http://127.0.0.1:9".into();
    let phone = h.phone(Some(Permission::View)).await;
    let m = h.connect(&phone).await.unwrap();
    match m.ollama(Method::Get, "/api/tags", vec![]).await {
        Err(ClientError::Remote(e)) => {
            assert_eq!(e.code, ErrorCode::NotFound);
            assert!(e.message.contains("isn't running"), "{}", e.message);
        }
        Ok(_) => panic!("expected an error"),
        Err(e) => panic!("expected a remote error, got {e}"),
    }
}
