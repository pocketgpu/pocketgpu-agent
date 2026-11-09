//! A fake push server for tests that send alerts.

use iroh::{PublicKey, Signature};
use pocketgpu_proto::push::{AlertRequest, signing_message};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Accepts HTTP posts, checks the agent signature, and passes the alerts on.
pub async fn fake_push_server() -> (String, mpsc::Receiver<(bool, AlertRequest)>) {
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
