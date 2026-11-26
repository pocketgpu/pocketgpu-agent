//! `pgpu/apps/1` (F42): the web apps running on a machine, and a local address in front of
//! one. [`Machine::forward_app`] listens on this device's `127.0.0.1` and sends each
//! connection a browser makes there to the app on the machine, over its own iroh connection
//! (direct when it can be, end-to-end encrypted always). Nothing here listens on the network.
//! A connection whose first request names another host (a web page that pointed its own
//! name at 127.0.0.1, DNS rebinding) is turned away before anything reaches the machine, and
//! on a phone so is one without the forward's key ([`Reach::Viewer`], `key.rs`).

mod key;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use iroh::EndpointAddr;
use iroh::endpoint::Connection;
use pocketgpu_proto::apps::{Apps, Request, Response};
use pocketgpu_proto::{alpn, read_frame, write_frame};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::{AbortHandle, JoinSet};

use crate::{Client, ClientError, Machine, request};

impl Machine {
    /// The apps running on the machine now; empty with `enabled: false` while they're off.
    pub async fn apps(&self) -> Result<Apps, ClientError> {
        let conn = self.apps.get(self, alpn::APPS).await?;
        match request(&conn, &Request::List).await? {
            Response::Apps(a) => Ok(a),
            Response::Error(e) => Err(ClientError::Remote(e)),
            Response::Opened | Response::Start { .. } => Err(ClientError::NoReply),
        }
    }

    /// Where to start in the app on `port`: `/`, or Jupyter's `/?token=…` (Shell). `/` from
    /// agents before it.
    pub async fn app_start(&self, port: u16) -> Result<String, ClientError> {
        let conn = self.apps.get(self, alpn::APPS).await?;
        match request(&conn, &Request::Start { port }).await {
            Ok(Response::Start { path }) if path.starts_with('/') => Ok(path),
            Ok(Response::Error(e)) if e.code != pocketgpu_proto::ErrorCode::Invalid => {
                Err(ClientError::Remote(e))
            }
            // An agent before `Start` can't read the request.
            Ok(_) | Err(ClientError::Frame(_)) => Ok("/".into()),
            Err(e) => Err(e),
        }
    }

    /// Puts the app on `port` at `127.0.0.1:local` here: the same port when it is free and
    /// `local` is `None`, any free one with `Some(0)`. `reach` says who may use it there;
    /// open it at [`Forward::url`]. Checks first that the machine lets this device in
    /// (Shell, apps on) and that the app answers. Stops when dropped.
    pub async fn forward_app(
        &self,
        port: u16,
        local: Option<u16>,
        reach: Reach,
    ) -> Result<Forward, ClientError> {
        let link = Link {
            client: self.client.clone(),
            addr: self.addr.clone(),
            conn: Arc::new(Mutex::new(None)),
        };
        // One connection opened and closed at once: the machine says now if it refuses.
        let (stream, _conn) = link.open(port).await?;
        drop(stream);
        let listener = bind(local.unwrap_or(port)).await?;
        let addr = listener
            .local_addr()
            .map_err(|e| ClientError::Connect(e.to_string()))?;
        let key = (reach == Reach::Viewer).then(|| Arc::new(key::Key::new(addr.port())));
        let task = tokio::spawn(serve(listener, link, port, addr.port(), key.clone()));
        Ok(Forward {
            local: addr,
            key,
            task: task.abort_handle(),
        })
    }
}

/// Who may use a forward's local address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Only the browser that opened [`Forward::url`]: it then carries the forward's key as
    /// a cookie. Phones, where every app shares 127.0.0.1.
    Viewer,
    /// Any program on this device: a laptop's own tools reach Jupyter at localhost:8888.
    Anyone,
}

/// An app put at a local address; stops listening, and ends its connections, when dropped.
#[derive(Debug)]
pub struct Forward {
    pub local: SocketAddr,
    key: Option<Arc<key::Key>>,
    task: AbortHandle,
}

impl Forward {
    /// The address a browser opens to start at `path` (`/`, Jupyter's `/?token=…`); with
    /// [`Reach::Viewer`] it carries the key once.
    pub fn url(&self, path: &str) -> String {
        match &self.key {
            Some(k) => format!("http://{}{}", self.local, k.entry(path)),
            None => format!("http://{}{path}", self.local),
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `want` on 127.0.0.1, else any free port there.
async fn bind(want: u16) -> Result<TcpListener, ClientError> {
    if want != 0
        && let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, want)).await
    {
        return Ok(l);
    }
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| ClientError::Connect(format!("can't listen here: {e}")))
}

/// The forward's own connection to the machine, dialled again when it drops (a new
/// network, the machine restarted).
#[derive(Clone)]
struct Link {
    client: Client,
    addr: EndpointAddr,
    conn: Arc<Mutex<Option<Connection>>>,
}

type Stream = (iroh::endpoint::SendStream, iroh::endpoint::RecvStream);

impl Link {
    async fn connection(&self) -> Result<Connection, ClientError> {
        let mut open = self.conn.lock().await;
        if let Some(c) = open.as_ref()
            && c.close_reason().is_none()
        {
            return Ok(c.clone());
        }
        let c = self.client.dial(self.addr.clone(), alpn::APPS).await?;
        *open = Some(c.clone());
        Ok(c)
    }

    /// A stream to the app that the machine answered `Opened` on.
    async fn open(&self, port: u16) -> Result<(Stream, Connection), ClientError> {
        let conn = self.connection().await?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        write_frame(&mut send, &Request::Open { port }).await?;
        match read_frame::<_, Response>(&mut recv).await? {
            Some(Response::Opened) => Ok(((send, recv), conn)),
            Some(Response::Error(e)) => Err(ClientError::Remote(e)),
            _ => Err(ClientError::NoReply),
        }
    }
}

/// The most of a request's head read before deciding; real ones are far smaller.
const HEAD_BYTES: usize = 32 * 1024;
/// How long a browser has to send its first request on a new connection.
const HEAD_WAIT: Duration = Duration::from_secs(60);

async fn serve(
    listener: TcpListener,
    link: Link,
    port: u16,
    local: u16,
    key: Option<Arc<key::Key>>,
) {
    // Every connection's task ends with this one: dropping a JoinSet aborts them.
    let mut pipes = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((mut tcp, _)) = accepted else { continue };
                let link = link.clone();
                let key = key.clone();
                pipes.spawn(async move {
                    let head = match tokio::time::timeout(HEAD_WAIT, read_head(&mut tcp)).await {
                        Ok(Some(head)) => head,
                        _ => return,
                    };
                    // Only the headers are read here: a body may follow them already.
                    let top = &head[..head_end(&head)];
                    let answer = if !host_allowed(top, local) {
                        Some(refused())
                    } else {
                        match key.as_deref().map(|k| k.check(top)) {
                            Some(key::Verdict::Answer(a)) => Some(a),
                            Some(key::Verdict::Pass) | None => None,
                        }
                    };
                    if let Some(answer) = answer {
                        let _ = tcp.write_all(answer.as_bytes()).await;
                        let _ = tcp.shutdown().await;
                        return;
                    }
                    let head = key::without_keys(head);
                    if let Ok((stream, _conn)) = link.open(port).await {
                        pipe(tcp, head, stream).await;
                    }
                });
            }
            Some(_) = pipes.join_next(), if !pipes.is_empty() => {}
        }
    }
}

/// The answer to a request for another host.
fn refused() -> String {
    let body = "Open this app at localhost or 127.0.0.1 only.\n";
    format!(
        "HTTP/1.1 421 Misdirected Request\r\ncontent-type: text/plain\r\nconnection: close\r\n\
         content-length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// The bytes up to the end of the first request's headers; `None` if the browser closes
/// first or sends more than [`HEAD_BYTES`] without ending them.
async fn read_head(tcp: &mut TcpStream) -> Option<Vec<u8>> {
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 4096];
    loop {
        let n = tcp.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        let from = head.len().saturating_sub(3);
        head.extend_from_slice(&buf[..n]);
        if head[from..].windows(4).any(|w| w == b"\r\n\r\n") {
            return Some(head);
        }
        if head.len() > HEAD_BYTES {
            return None;
        }
    }
}

/// Where the first request's headers end in `head`, past the empty line; all of it if they
/// don't.
fn head_end(head: &[u8]) -> usize {
    head.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(head.len(), |at| at + 4)
}

/// Whether the request's `Host` is this device itself: `127.0.0.1`, `localhost`, `[::1]` (or
/// `0.0.0.0`, which some tools send), with no port or the one listened on. A page from
/// anywhere else always names its own host.
fn host_allowed(head: &[u8], local: u16) -> bool {
    let Ok(text) = std::str::from_utf8(head) else {
        return false;
    };
    let mut hosts = text
        .split("\r\n")
        .skip(1)
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim());
    let (Some(host), None) = (hosts.next(), hosts.next()) else {
        return false;
    };
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) if !n.is_empty() && !p.contains(']') => (n, Some(p)),
        _ => (host, None),
    };
    let ours = ["127.0.0.1", "localhost", "[::1]", "0.0.0.0"]
        .iter()
        .any(|o| name.eq_ignore_ascii_case(o));
    ours && port.is_none_or(|p| p.parse() == Ok(local))
}

/// Bytes both ways until both sides end; each side's end is passed on to the other. `head`,
/// already read from the browser, goes first.
async fn pipe(tcp: TcpStream, head: Vec<u8>, (mut send, mut recv): Stream) {
    let (mut from_browser, mut to_browser) = tcp.into_split();
    let up = async {
        if send.write_all(&head).await.is_ok() {
            let _ = tokio::io::copy(&mut from_browser, &mut send).await;
        }
        let _ = send.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut to_browser).await;
        let _ = to_browser.shutdown().await;
    };
    tokio::join!(up, down);
}

#[cfg(test)]
mod tests {
    use super::{head_end, host_allowed};

    fn req(host: &str) -> Vec<u8> {
        format!("GET / HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\n\r\n").into_bytes()
    }

    #[test]
    fn this_device_is_allowed() {
        for h in [
            "127.0.0.1:8888",
            "localhost:8888",
            "[::1]:8888",
            "LocalHost:8888",
            "localhost",
            "[::1]",
        ] {
            assert!(host_allowed(&req(h), 8888), "{h}");
        }
    }

    #[test]
    fn other_hosts_and_ports_are_refused() {
        for h in [
            "evil.example:8888",
            "127.0.0.1.nip.io:8888",
            "localhost:9999",
            "",
            "localhost:x",
        ] {
            assert!(!host_allowed(&req(h), 8888), "{h}");
        }
        assert!(!host_allowed(
            b"GET / HTTP/1.1\r\nAccept: */*\r\n\r\n",
            8888
        ));
        let two = b"GET / HTTP/1.1\r\nHost: localhost:8888\r\nHost: evil.example\r\n\r\n";
        assert!(!host_allowed(two, 8888));
    }

    #[test]
    fn a_body_read_with_the_head_is_left_out_of_it() {
        // An upload's first bytes often come in the same read as its headers.
        let mut head = req("127.0.0.1:8888");
        let len = head.len();
        head.extend_from_slice(b"\x89PNG\r\n\x1a\n\xff");
        assert_eq!(head_end(&head), len);
        assert!(host_allowed(&head[..head_end(&head)], 8888));
        assert_eq!(head_end(b"GET / HTTP/1.1\r\n"), 16);
    }
}
