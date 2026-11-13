//! Finding the web apps that run on this machine: each known port, and each port added with
//! `pgpu apps add`, is tried on `localhost`; one that answers gets one plain `GET /` (and one
//! redirect followed), and its page says what it is. Only this machine's own loopback
//! addresses are ever tried.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use pocketgpu_proto::apps::{App, KNOWN, Kind, NEVER, kind_of};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::store::config::AddedApp;

/// How long a port has to answer a connection.
const CONNECT: Duration = Duration::from_millis(300);
/// How long the page has to come.
const PAGE: Duration = Duration::from_millis(1500);
/// The most of a page read for its title.
const PAGE_BYTES: usize = 64 * 1024;

/// The ports looked at: the known ones and the added ones, never the refused ones.
pub fn ports(added: &[AddedApp]) -> BTreeSet<u16> {
    KNOWN
        .iter()
        .map(|(p, _)| *p)
        .chain(added.iter().map(|a| a.port))
        .filter(|p| !NEVER.contains(p))
        .collect()
}

/// Connects to the app on `port` on this machine, IPv4 first, then IPv6.
pub async fn connect(port: u16) -> std::io::Result<TcpStream> {
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let mut last = std::io::Error::other("nothing answers there");
    for addr in [v4, v6] {
        match tokio::time::timeout(CONNECT, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = e,
            Err(_) => last = std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer"),
        }
    }
    Err(last)
}

/// Every app running now, by port, with what `pgpu apps` set.
pub async fn find(added: &[AddedApp], off: &[u16]) -> Vec<App> {
    let mut looks = tokio::task::JoinSet::new();
    for port in ports(added) {
        looks.spawn(async move { (port, look(port).await) });
    }
    let mut apps = Vec::new();
    while let Some(done) = looks.join_next().await {
        if let Ok((port, Some(page))) = done {
            let given = added.iter().find(|a| a.port == port);
            apps.push(describe(port, &page, given, off.contains(&port)));
        }
    }
    apps.sort_by_key(|a| a.port);
    apps
}

/// What one app is, from its page.
pub fn describe(port: u16, page: &str, given: Option<&AddedApp>, off: bool) -> App {
    let kind = kind_of(port, page);
    let name = given
        .and_then(|a| a.name.clone())
        .or_else(|| (kind == Kind::Other).then(|| title(page)).flatten())
        .unwrap_or_else(|| kind.name().to_string());
    App {
        port,
        name,
        kind,
        added: given.is_some(),
        off,
    }
}

/// The page on `port`, or `None` when nothing answers there. A port that answers but sends
/// no page still counts, with an empty one.
pub async fn page(port: u16) -> Option<String> {
    look(port).await
}

async fn look(port: u16) -> Option<String> {
    let first = get(port, "/").await?;
    // Jupyter and others answer `/` with a redirect to their real page.
    match location(&first) {
        Some(path) => Some(get(port, &path).await.unwrap_or(first)),
        None => Some(first),
    }
}

async fn get(port: u16, path: &str) -> Option<String> {
    let mut s = connect(port).await.ok()?;
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: localhost:{port}\r\nUser-Agent: PocketGPU\r\nAccept: text/html\r\n\r\n"
    );
    let read = async {
        s.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        while buf.len() < PAGE_BYTES {
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Ok::<_, std::io::Error>(buf)
    };
    let page = match tokio::time::timeout(PAGE, read).await {
        Ok(Ok(b)) => String::from_utf8_lossy(&b).into_owned(),
        _ => String::new(),
    };
    Some(page)
}

/// A same-site redirect's path, from a `30x` answer.
fn location(page: &str) -> Option<String> {
    let head = page.split("\r\n\r\n").next()?;
    let status = head.lines().next()?;
    if !status.split(' ').nth(1).is_some_and(|c| c.starts_with('3')) {
        return None;
    }
    let loc = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("location").then(|| v.trim())
    })?;
    // Only a path on the same app, never somewhere else.
    let path = match loc.find("://") {
        Some(i) => {
            let rest = &loc[i + 3..];
            let host = rest.split('/').next().unwrap_or("");
            if !host.starts_with("localhost") && !host.starts_with("127.0.0.1") {
                return None;
            }
            &rest[host.len()..]
        }
        None => loc,
    };
    path.starts_with('/').then(|| path.to_string())
}

/// The page's `<title>`, trimmed, if it has one worth showing.
fn title(page: &str) -> Option<String> {
    let lower = page.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open = start + lower[start..].find('>')? + 1;
    let end = open + lower[open..].find("</title")?;
    let t: String = page[open..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let t: String = t.chars().take(40).collect();
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_stay_on_the_app() {
        let r = "HTTP/1.1 302 Found\r\nLocation: /lab?\r\n\r\n";
        assert_eq!(location(r).as_deref(), Some("/lab?"));
        let abs = "HTTP/1.1 302 Found\r\nlocation: http://localhost:8888/tree\r\n\r\n";
        assert_eq!(location(abs).as_deref(), Some("/tree"));
        let away = "HTTP/1.1 302 Found\r\nLocation: https://evil.example/x\r\n\r\n";
        assert_eq!(location(away), None);
        assert_eq!(location("HTTP/1.1 200 OK\r\nLocation: /x\r\n\r\n"), None);
    }

    #[test]
    fn names_come_from_the_page_or_the_port() {
        let p = "HTTP/1.1 200 OK\r\n\r\n<html><head><title>\n  My  demo </title>";
        assert_eq!(describe(9000, p, None, false).name, "My demo");
        assert_eq!(describe(6006, "", None, false).name, "TensorBoard");
        let jl = "<title>JupyterLab</title>";
        assert_eq!(describe(8888, jl, None, true).name, "Jupyter Lab");
        let given = AddedApp {
            port: 9000,
            name: Some("Demo".into()),
        };
        let a = describe(9000, p, Some(&given), false);
        assert_eq!((a.name.as_str(), a.added), ("Demo", true));
    }

    #[test]
    fn refused_ports_are_never_looked_at() {
        let added = [AddedApp {
            port: 22,
            name: None,
        }];
        assert!(!ports(&added).contains(&22));
        assert!(ports(&[]).contains(&8888));
    }

    #[tokio::test]
    async fn a_running_app_is_found() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.0 200 OK\r\n\r\n<title>Gradio</title>")
                    .await;
            }
        });
        let added = [AddedApp { port, name: None }];
        let apps = find(&added, &[]).await;
        let app = apps.iter().find(|a| a.port == port).unwrap();
        assert_eq!((app.kind, app.added), (Kind::Gradio, true));
    }
}
