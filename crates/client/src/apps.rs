//! `pgpu/apps/1` (F42): the web apps running on a machine, and a local address in front of
//! one. [`Machine::forward_app`] listens on this device's `127.0.0.1` and sends each
//! connection a browser makes there to the app on the machine, over its own iroh connection
//! (direct when it can be, end-to-end encrypted always). Nothing here listens on the network.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use iroh::EndpointAddr;
use iroh::endpoint::Connection;
use pocketgpu_proto::apps::{Apps, Request, Response};
use pocketgpu_proto::{alpn, read_frame, write_frame};
use tokio::io::AsyncWriteExt;
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
            Response::Opened => Err(ClientError::NoReply),
        }
    }

    /// Puts the app on `port` at `127.0.0.1:local` here: the same port when it is free and
    /// `local` is `None`, any free one with `Some(0)`. Checks first that the machine lets
    /// this device in (Shell, apps on) and that the app answers. Stops when dropped.
    pub async fn forward_app(&self, port: u16, local: Option<u16>) -> Result<Forward, ClientError> {
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
        let task = tokio::spawn(serve(listener, link, port));
        Ok(Forward {
            local: addr,
            task: task.abort_handle(),
        })
    }
}

/// An app put at a local address; stops listening, and ends its connections, when dropped.
#[derive(Debug)]
pub struct Forward {
    pub local: SocketAddr,
    task: AbortHandle,
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

async fn serve(listener: TcpListener, link: Link, port: u16) {
    // Every connection's task ends with this one: dropping a JoinSet aborts them.
    let mut pipes = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((tcp, _)) = accepted else { continue };
                let link = link.clone();
                pipes.spawn(async move {
                    if let Ok((stream, _conn)) = link.open(port).await {
                        pipe(tcp, stream).await;
                    }
                });
            }
            Some(_) = pipes.join_next(), if !pipes.is_empty() => {}
        }
    }
}

/// Bytes both ways until both sides end; each side's end is passed on to the other.
async fn pipe(tcp: TcpStream, (mut send, mut recv): Stream) {
    let (mut from_browser, mut to_browser) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut from_browser, &mut send).await;
        let _ = send.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut to_browser).await;
        let _ = to_browser.shutdown().await;
    };
    tokio::join!(up, down);
}
