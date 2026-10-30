//! Is Ollama running? A plain HTTP request to its `/api/version`, so no HTTP client is needed.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TIMEOUT: Duration = Duration::from_secs(2);

/// Ollama's version when it answers at `addr` (such as `http://127.0.0.1:11434`), else `None`.
pub async fn version(addr: &str) -> Option<String> {
    let host = host_port(addr)?;
    tokio::time::timeout(TIMEOUT, async {
        let mut stream = TcpStream::connect(&host).await.ok()?;
        let req = format!("GET /api/version HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::with_capacity(512);
        stream.take(16 * 1024).read_to_end(&mut buf).await.ok()?;
        let text = String::from_utf8_lossy(&buf);
        let (head, body) = text.split_once("\r\n\r\n")?;
        if !head.starts_with("HTTP/1.") || !head.split(' ').nth(1)?.starts_with('2') {
            return None;
        }
        let json: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
        Some(json.get("version")?.as_str()?.to_string())
    })
    .await
    .ok()
    .flatten()
}

/// `http://host:port/…` → `host:port`. Only plain HTTP: Ollama listens locally.
fn host_port(addr: &str) -> Option<String> {
    let rest = addr.strip_prefix("http://")?;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:80")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn serve_once(reply: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = s.read(&mut buf).await;
            s.write_all(reply.as_bytes()).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn reads_the_version() {
        let addr = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"version\":\"0.12.3\"}",
        )
        .await;
        assert_eq!(version(&addr).await.as_deref(), Some("0.12.3"));
    }

    #[tokio::test]
    async fn errors_and_silence_mean_down() {
        let addr = serve_once("HTTP/1.1 500 Oops\r\n\r\n{}").await;
        assert_eq!(version(&addr).await, None);
        assert_eq!(version("http://127.0.0.1:1").await, None);
        assert_eq!(version("https://example.com").await, None);
    }

    #[test]
    fn parses_addresses() {
        assert_eq!(
            host_port("http://127.0.0.1:11434").as_deref(),
            Some("127.0.0.1:11434")
        );
        assert_eq!(host_port("http://box/x").as_deref(), Some("box:80"));
    }
}
