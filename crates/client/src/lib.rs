//! Connects to PocketGPU agents and speaks the phone ↔ agent protocol.
//!
//! Used by the phone app's Rust core; `local` is the desktop tray app's way to the agent on
//! the same machine (1.2).

mod docker;
mod events;
pub mod local;
mod shell;
mod tunnel;

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl, SecretKey};
use pocketgpu_proto::msg::{control, jobs, metrics, pair};
use pocketgpu_proto::pairing::{PairCode, PairInput, proof};
use pocketgpu_proto::{FrameError, Platform, Sample, alpn, read_frame, write_frame};
use serde::{Serialize, de::DeserializeOwned};

pub use docker::ContainerLog;
pub use events::EventStream;
pub use shell::{ShellEvent, ShellOpened, ShellReceiver, ShellSender};
pub use tunnel::OllamaReply;

/// How long to wait for a machine before showing it as unreachable.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
/// Pairing may first have to look the machine up by its code, which takes longer.
pub const PAIR_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("could not reach the machine within {0:?}")]
    Timeout(Duration),
    #[error("could not connect: {0}")]
    Connect(String),
    #[error("connection lost: {0}")]
    Connection(String),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("the machine refused: {0}")]
    Remote(pocketgpu_proto::Error),
    #[error("the machine closed the stream without replying")]
    NoReply,
    #[error("the machine answered with a different key than its QR code; don't pair with it")]
    WrongMachine,
    /// The agent doesn't offer this protocol: it is older than the feature.
    #[error("this machine's agent is too old for this. Update it with `pgpu update`")]
    Unsupported,
}

/// Which relays the phone uses.
#[derive(Debug, Clone)]
pub enum Relays {
    /// Our relays in releases (`PGPU_DEFAULT_RELAYS`), n0's public ones in development.
    Default,
    Custom(Vec<RelayUrl>),
    /// No relays: tests on one machine.
    Disabled,
}

/// Whether traffic currently goes straight to the machine or through a relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionType {
    Direct,
    Relayed,
    Unknown,
}

/// The phone's network endpoint. One per app.
#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
}

impl Client {
    pub async fn new(secret: SecretKey, relays: Relays) -> Result<Self, ClientError> {
        let builder = match relays {
            Relays::Disabled => Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled),
            Relays::Default => {
                let ours: Vec<RelayUrl> = pocketgpu_proto::default_relays()
                    .into_iter()
                    .filter_map(|u| u.parse().ok())
                    .collect();
                let builder = Endpoint::builder(presets::N0);
                if ours.is_empty() {
                    builder
                } else {
                    builder.relay_mode(RelayMode::custom(ours))
                }
            }
            Relays::Custom(urls) => {
                Endpoint::builder(presets::N0).relay_mode(RelayMode::custom(urls))
            }
        };
        // HTTPS_PROXY and friends, for desktops behind a company proxy (F31); phones have none.
        let endpoint = builder
            .secret_key(secret)
            .proxy_from_env()
            .bind()
            .await
            .map_err(|e| ClientError::Connect(e.to_string()))?;
        Ok(Self { endpoint })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Opens the control connection to a machine, giving up after [`CONNECT_TIMEOUT`].
    pub async fn connect(&self, addr: impl Into<EndpointAddr>) -> Result<Machine, ClientError> {
        let addr = addr.into();
        let control = self.dial(addr.clone(), alpn::CONTROL).await?;
        Ok(Machine {
            client: self.clone(),
            addr,
            control,
            metrics: Shared::default(),
            jobs: Shared::default(),
            docker: Shared::default(),
        })
    }

    /// Starts pairing with a scanned QR code or a typed code. A QR code dials the machine
    /// directly; a typed code dials the short-lived endpoint derived from it.
    pub async fn pair(&self, input: &PairInput, info: PhoneInfo) -> Result<Pairing, ClientError> {
        let (addr, expected) = match input {
            PairInput::Qr(qr) => {
                let id: PublicKey = qr
                    .agent_id
                    .parse()
                    .map_err(|_| ClientError::Connect("the QR code is damaged".into()))?;
                let mut addr = EndpointAddr::new(id);
                for r in &qr.relays {
                    if let Ok(url) = r.parse::<RelayUrl>() {
                        addr = addr.with_relay_url(url);
                    }
                }
                (addr, Some(id))
            }
            PairInput::Code(code) => (EndpointAddr::new(rendezvous_id(code)), None),
        };
        self.pair_at(addr, input.code(), expected, info).await
    }

    /// Pairs with the endpoint at `addr`. `expected` is the agent ID the phone already
    /// trusts (from a QR code); the machine must then answer with that key.
    pub async fn pair_at(
        &self,
        addr: EndpointAddr,
        code: PairCode,
        expected: Option<PublicKey>,
        info: PhoneInfo,
    ) -> Result<Pairing, ClientError> {
        let target = addr.id;
        let conn = self
            .dial_within(addr, alpn::PAIR, PAIR_CONNECT_TIMEOUT)
            .await?;
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        let req = pair::PairRequest {
            secret_proof: proof(
                &code.secret(),
                target.as_bytes(),
                self.endpoint.id().as_bytes(),
            ),
            device_name: info.device_name,
            platform: info.platform,
            push_ticket: info.push_ticket,
        };
        write_frame(&mut send, &req).await?;
        send.finish()
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        Ok(Pairing {
            conn,
            recv,
            expected,
        })
    }

    pub async fn dial(&self, addr: EndpointAddr, alpn: &[u8]) -> Result<Connection, ClientError> {
        self.dial_within(addr, alpn, CONNECT_TIMEOUT).await
    }

    async fn dial_within(
        &self,
        addr: EndpointAddr,
        alpn: &[u8],
        limit: Duration,
    ) -> Result<Connection, ClientError> {
        match tokio::time::timeout(limit, self.endpoint.connect(addr, alpn)).await {
            Ok(Ok(conn)) => Ok(conn),
            Ok(Err(e)) => {
                let text = e.to_string();
                // TLS alert 120 (no_application_protocol): the agent has no handler for `alpn`.
                if text.contains("error 120") {
                    Err(ClientError::Unsupported)
                } else {
                    Err(ClientError::Connect(text))
                }
            }
            Err(_) => Err(ClientError::Timeout(limit)),
        }
    }
}

/// An open connection to one machine.
#[derive(Debug, Clone)]
pub struct Machine {
    client: Client,
    addr: EndpointAddr,
    control: Connection,
    metrics: Shared,
    jobs: Shared,
    docker: Shared,
}

/// A connection for one more protocol, opened on first use and then shared by every
/// request and stream on it: a new one costs several round trips through a far relay.
#[derive(Debug, Clone, Default)]
struct Shared(Arc<tokio::sync::Mutex<Option<Connection>>>);

impl Shared {
    async fn get(&self, m: &Machine, alpn: &[u8]) -> Result<Connection, ClientError> {
        let mut open = self.0.lock().await;
        if let Some(c) = open.as_ref()
            && c.close_reason().is_none()
        {
            return Ok(c.clone());
        }
        let conn = m.client.dial(m.addr.clone(), alpn).await?;
        *open = Some(conn.clone());
        Ok(conn)
    }

    fn close(&self) {
        if let Ok(open) = self.0.try_lock()
            && let Some(c) = open.as_ref()
        {
            c.close(0u32.into(), b"bye");
        }
    }
}

impl Machine {
    pub fn connection_type(&self) -> ConnectionType {
        connection_type(&self.control)
    }

    /// Sends one control request and waits for its reply.
    pub async fn control(&self, req: control::Request) -> Result<control::Response, ClientError> {
        match request(&self.control, &req).await? {
            control::Response::Error(e) => Err(ClientError::Remote(e)),
            other => Ok(other),
        }
    }

    pub async fn hello(
        &self,
        app_version: &str,
    ) -> Result<pocketgpu_proto::MachineInfo, ClientError> {
        let req = control::Request::Hello {
            app_version: app_version.into(),
            proto: pocketgpu_proto::msg::PROTO_VERSION,
        };
        match self.control(req).await? {
            control::Response::MachineInfo(info) => Ok(info),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Everything the machines list shows in one round trip. Agents before `Status` answer
    /// with an error: the caller then asks the old way.
    pub async fn status(&self, app_version: &str) -> Result<control::Status, ClientError> {
        let req = control::Request::Status {
            app_version: app_version.into(),
            proto: pocketgpu_proto::msg::PROTO_VERSION,
        };
        match self.control(req).await? {
            control::Response::Status(status) => Ok(*status),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Sends one jobs request and waits for its reply.
    pub async fn jobs(&self, req: jobs::Request) -> Result<jobs::Response, ClientError> {
        let conn = self.jobs.get(self, alpn::JOBS).await?;
        match request(&conn, &req).await? {
            jobs::Response::Error(e) => Err(ClientError::Remote(e)),
            other => Ok(other),
        }
    }

    /// A job's output from `from_offset`, then new output as it's written, until the job ends.
    pub async fn follow_log(&self, id: u64, from_offset: u64) -> Result<LogStream, ClientError> {
        let conn = self.jobs.get(self, alpn::JOBS).await?;
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        write_frame(&mut send, &jobs::Request::FollowLog { id, from_offset }).await?;
        send.finish()
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        Ok(LogStream { _conn: conn, recv })
    }

    /// Live samples until the returned stream is dropped.
    pub async fn subscribe(&self, interval: Duration) -> Result<MetricsStream, ClientError> {
        let conn = self.metrics.get(self, alpn::METRICS).await?;
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        let sub = metrics::Subscribe {
            interval_ms: interval.as_millis().min(u32::MAX as u128) as u32,
        };
        write_frame(&mut send, &sub).await?;
        Ok(MetricsStream {
            _conn: conn,
            _send: send,
            recv,
        })
    }

    /// The machine's direct (IP) addresses on this connection, the one in use first: worth
    /// saving and offering on the next connect, so it can go direct before the relay has
    /// found the machine.
    pub fn direct_addrs(&self) -> Vec<std::net::SocketAddr> {
        let paths = self.control.paths();
        let mut addrs: Vec<(bool, std::net::SocketAddr)> = paths
            .iter()
            .filter_map(|p| match p.remote_addr() {
                iroh::TransportAddr::Ip(a) => Some((p.is_selected(), *a)),
                _ => None,
            })
            .collect();
        addrs.sort_by_key(|(selected, _)| !selected);
        let mut out: Vec<std::net::SocketAddr> = Vec::new();
        for (_, a) in addrs {
            if !out.contains(&a) {
                out.push(a);
            }
        }
        out
    }

    /// Round-trip time on the path in use.
    pub fn latency(&self) -> Option<Duration> {
        let paths = self.control.paths();
        paths.iter().find(|p| p.is_selected()).map(|p| p.rtt())
    }

    /// Whether the machine closed the connection because its agent stopped cleanly.
    pub fn ended(&self) -> bool {
        matches!(
            self.control.close_reason(),
            Some(iroh::endpoint::ConnectionError::ApplicationClosed(c))
                if c.reason.as_ref() == pocketgpu_proto::ENDED_REASON
        )
    }

    /// Whether the connection has ended, so a new one is needed.
    pub fn is_closed(&self) -> bool {
        self.control.close_reason().is_some()
    }

    pub fn close(&self) {
        self.control.close(0u32.into(), b"bye");
        for s in [&self.metrics, &self.jobs, &self.docker] {
            s.close();
        }
    }
}

/// What a followed log sends.
#[derive(Debug, Clone, PartialEq)]
pub enum LogEvent {
    Chunk {
        offset: u64,
        bytes: Vec<u8>,
    },
    /// The job ended and all its output was sent.
    End {
        exit_code: Option<i32>,
    },
}

pub struct LogStream {
    _conn: Connection,
    recv: iroh::endpoint::RecvStream,
}

impl LogStream {
    /// The next chunk or the end; `None` if the machine closed the stream early.
    pub async fn next(&mut self) -> Result<Option<LogEvent>, ClientError> {
        match read_frame::<_, jobs::Response>(&mut self.recv).await? {
            Some(jobs::Response::LogChunk { offset, bytes }) => {
                Ok(Some(LogEvent::Chunk { offset, bytes }))
            }
            Some(jobs::Response::End { exit_code }) => Ok(Some(LogEvent::End { exit_code })),
            Some(jobs::Response::Error(e)) => Err(ClientError::Remote(e)),
            Some(_) | None => Ok(None),
        }
    }
}

pub struct MetricsStream {
    _conn: Connection,
    _send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
}

impl MetricsStream {
    /// The next sample, or `None` when the machine ends the stream.
    pub async fn next(&mut self) -> Result<Option<Sample>, ClientError> {
        match read_frame::<_, metrics::Event>(&mut self.recv).await? {
            Some(metrics::Event::Sample(s)) => Ok(Some(s)),
            Some(metrics::Event::Error(e)) => Err(ClientError::Remote(e)),
            None => Ok(None),
        }
    }
}

/// What the phone tells a machine about itself when pairing.
#[derive(Debug, Clone)]
pub struct PhoneInfo {
    pub device_name: String,
    pub platform: Platform,
    pub push_ticket: Option<String>,
}

/// The ID of the endpoint an agent listens on while `code` is active.
pub fn rendezvous_id(code: &PairCode) -> PublicKey {
    SecretKey::from_bytes(&code.rendezvous_seed()).public()
}

/// A pairing in progress.
pub struct Pairing {
    conn: Connection,
    recv: iroh::endpoint::RecvStream,
    expected: Option<PublicKey>,
}

impl Pairing {
    /// The machine's next reply: `Pending` first, then `Approved` or `Denied`.
    /// A refused code comes back as [`ClientError::Remote`].
    pub async fn next(&mut self) -> Result<pair::Response, ClientError> {
        match read_frame::<_, pair::Response>(&mut self.recv).await? {
            Some(pair::Response::Error(e)) => Err(ClientError::Remote(e)),
            Some(pair::Response::Pending {
                agent_id,
                agent_name,
            }) => {
                let id = PublicKey::from_bytes(&agent_id).map_err(|_| ClientError::WrongMachine)?;
                if self.expected.is_some_and(|e| e != id) {
                    self.conn.close(1u32.into(), b"wrong machine");
                    return Err(ClientError::WrongMachine);
                }
                self.expected = Some(id);
                Ok(pair::Response::Pending {
                    agent_id,
                    agent_name,
                })
            }
            Some(other) => {
                if matches!(
                    other,
                    pair::Response::Approved { .. } | pair::Response::Denied
                ) {
                    self.conn.close(0u32.into(), b"done");
                }
                Ok(other)
            }
            None => Err(ClientError::NoReply),
        }
    }

    /// The machine's ID, known once it replied `Pending`.
    pub fn agent_id(&self) -> Option<PublicKey> {
        self.expected
    }
}

async fn request<Req, Resp>(conn: &Connection, req: &Req) -> Result<Resp, ClientError>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| ClientError::Connection(e.to_string()))?;
    write_frame(&mut send, req).await?;
    send.finish()
        .map_err(|e| ClientError::Connection(e.to_string()))?;
    read_frame(&mut recv).await?.ok_or(ClientError::NoReply)
}

fn connection_type(conn: &Connection) -> ConnectionType {
    let paths = conn.paths();
    match paths.iter().find(|p| p.is_selected()) {
        Some(p) if p.is_relay() => ConnectionType::Relayed,
        Some(_) => ConnectionType::Direct,
        None => ConnectionType::Unknown,
    }
}
