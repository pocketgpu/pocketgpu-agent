//! `pgpu/pair-lan/1` (F47): pairing a phone that found this computer on the Wi-Fi.
//!
//! There is no code to prove, so the request is only heard while `pgpu pair` waits, only over
//! a direct path to a private address, one at a time, and three refusals in ten minutes shut
//! it for ten minutes. Someone at the computer always decides (`cli/pair.rs` never answers a
//! Wi-Fi ask in advance), with the phone's key in front of them.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use pocketgpu_proto::lan::LanPairRequest;
use pocketgpu_proto::msg::pair::Response;
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};
use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::pair::code::Ask;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Refusals (a no, or no answer) that close pairing from the Wi-Fi for a while.
pub const MAX_REFUSALS: usize = 3;
pub const REFUSAL_WINDOW: Duration = Duration::from_secs(10 * 60);
/// How long a new connection may take to find its direct path.
const DIRECT_WAIT: Duration = Duration::from_secs(2);

/// One ask at a time, and the recent refusals.
#[derive(Debug, Default)]
pub struct Gate {
    inner: Mutex<GateState>,
}

#[derive(Debug, Default)]
struct GateState {
    pending: bool,
    refusals: VecDeque<Instant>,
}

/// Why a request from the Wi-Fi can't be heard now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    Busy,
    TooMany,
}

impl Gate {
    fn state(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.inner.lock().expect("lan gate poisoned")
    }

    /// Takes the one place for an ask. Release it with [`Gate::done`].
    pub fn enter(&self, now: Instant) -> Result<(), Closed> {
        let mut s = self.state();
        while s
            .refusals
            .front()
            .is_some_and(|t| now.duration_since(*t) >= REFUSAL_WINDOW)
        {
            s.refusals.pop_front();
        }
        if s.refusals.len() >= MAX_REFUSALS {
            return Err(Closed::TooMany);
        }
        if s.pending {
            return Err(Closed::Busy);
        }
        s.pending = true;
        Ok(())
    }

    /// Frees the place; a refusal counts towards closing.
    pub fn done(&self, now: Instant, refused: bool) {
        let mut s = self.state();
        s.pending = false;
        if refused {
            s.refusals.push_back(now);
        }
    }
}

/// Private, link-local and loopback addresses: the same network (or this computer), not the
/// internet. Shared address space (100.64/10, used by carriers and some VPNs) doesn't count.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[derive(Debug, Clone)]
pub struct LanPairHandler {
    agent: Arc<Agent>,
}

impl LanPairHandler {
    pub fn new(agent: Arc<Agent>) -> Self {
        Self { agent }
    }
}

impl ProtocolHandler for LanPairHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let device = conn.remote_id();
        let on = self.agent.config.read().expect("config lock poisoned").lan;
        if !on || self.agent.pairing.lan_asks().is_none() {
            conn.close(403u32.into(), b"not waiting for a phone");
            return Ok(());
        }
        if !direct_and_local(&conn).await {
            tracing::info!(remote = %device.fmt_short(), "refused Wi-Fi pairing: not a direct local path");
            conn.close(403u32.into(), b"not on this network");
            return Ok(());
        }
        if let Err(e) = serve(&self.agent, device, &conn).await {
            tracing::debug!(remote = %device.fmt_short(), "Wi-Fi pairing ended: {e:#}");
        }
        Ok(())
    }
}

/// Whether the connection runs over a direct path to a local address.
async fn direct_and_local(conn: &Connection) -> bool {
    let start = Instant::now();
    loop {
        let paths = conn.paths();
        let selected = paths.iter().find(|p| p.is_selected());
        if let Some(p) = selected {
            return matches!(p.remote_addr(), iroh::TransportAddr::Ip(a) if is_local(a.ip()));
        }
        if start.elapsed() >= DIRECT_WAIT {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn serve(agent: &Arc<Agent>, device: iroh::PublicKey, conn: &Connection) -> Result<()> {
    let (mut send, mut recv) = timeout(REQUEST_TIMEOUT, conn.accept_bi())
        .await
        .context("no request in time")??;
    let req: LanPairRequest = timeout(REQUEST_TIMEOUT, read_frame(&mut recv))
        .await
        .context("no request in time")??
        .context("stream closed before the request")?;

    if let Err(closed) = agent.pairing.lan.enter(Instant::now()) {
        let reply = Response::Error(match closed {
            Closed::Busy => Error::new(
                ErrorCode::Busy,
                "another phone is asking this computer. Try again in a minute",
            ),
            Closed::TooMany => Error::new(
                ErrorCode::Forbidden,
                "this computer said no several times. Try again in 10 minutes, or type the code",
            ),
        });
        return finish(conn, &mut send, &reply).await;
    }
    let result = ask(agent, device, req, conn, &mut send).await;
    agent
        .pairing
        .lan
        .done(Instant::now(), !matches!(result, Ok(true)));
    result.map(|_| ())
}

/// Puts the phone in front of the person at the computer. `Ok(true)` when it was paired.
async fn ask(
    agent: &Arc<Agent>,
    device: iroh::PublicKey,
    req: LanPairRequest,
    conn: &Connection,
    send: &mut iroh::endpoint::SendStream,
) -> Result<bool> {
    let Some(asks) = agent.pairing.lan_asks() else {
        let reply = Response::Error(Error::new(
            ErrorCode::Forbidden,
            "the computer stopped waiting for a phone. Run pgpu pair there again",
        ));
        finish(conn, send, &reply).await?;
        return Ok(false);
    };
    write_frame(
        send,
        &Response::Pending {
            agent_id: *agent.identity.agent_id().as_bytes(),
            agent_name: agent.name(),
        },
    )
    .await?;

    let (decision, decided) = oneshot::channel();
    let ask = Ask {
        device,
        device_name: req.device_name,
        platform: req.platform,
        push_ticket: req
            .push_ticket
            .filter(|t| t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit())),
        decision,
        from_lan: true,
    };
    // The session hears one ask at a time; a full queue means someone else is asking.
    if asks.try_send(ask).is_err() {
        let reply = Response::Error(Error::new(
            ErrorCode::Busy,
            "another phone is asking this computer. Try again in a minute",
        ));
        finish(conn, send, &reply).await?;
        return Ok(false);
    }
    let (reply, paired) = match decided.await {
        Ok(Some(permission)) => (
            Response::Approved {
                permission,
                relays: agent
                    .config
                    .read()
                    .expect("config lock poisoned")
                    .relays
                    .clone(),
            },
            true,
        ),
        _ => {
            if let Err(e) = audit::record(
                &agent.db(),
                &device.to_string(),
                Event::Refused,
                "pair",
                "on this Wi-Fi",
            ) {
                tracing::warn!("could not write audit entry: {e}");
            }
            (Response::Denied, false)
        }
    };
    finish(conn, send, &reply).await?;
    Ok(paired)
}

async fn finish(
    conn: &Connection,
    send: &mut iroh::endpoint::SendStream,
    reply: &Response,
) -> Result<()> {
    write_frame(send, reply).await?;
    send.finish()?;
    let _ = timeout(Duration::from_secs(5), conn.closed()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_ask_at_a_time() {
        let g = Gate::default();
        let t = Instant::now();
        assert_eq!(g.enter(t), Ok(()));
        assert_eq!(g.enter(t), Err(Closed::Busy));
        g.done(t, false);
        assert_eq!(g.enter(t), Ok(()));
    }

    #[test]
    fn three_noes_close_it_for_ten_minutes() {
        let g = Gate::default();
        let t = Instant::now();
        for _ in 0..MAX_REFUSALS {
            g.enter(t).unwrap();
            g.done(t, true);
        }
        assert_eq!(g.enter(t), Err(Closed::TooMany));
        assert_eq!(
            g.enter(t + REFUSAL_WINDOW - Duration::from_secs(1)),
            Err(Closed::TooMany)
        );
        assert_eq!(g.enter(t + REFUSAL_WINDOW), Ok(()));
    }

    #[test]
    fn a_yes_does_not_count_as_a_refusal() {
        let g = Gate::default();
        let t = Instant::now();
        for _ in 0..5 {
            g.enter(t).unwrap();
            g.done(t, false);
        }
        assert_eq!(g.enter(t), Ok(()));
    }

    #[test]
    fn only_local_addresses_count() {
        for ip in [
            "192.168.1.20",
            "10.0.0.5",
            "172.16.3.4",
            "169.254.1.1",
            "fe80::1",
            "fd12::1",
        ] {
            assert!(is_local(ip.parse().unwrap()), "{ip}");
        }
        assert!(is_local("127.0.0.1".parse().unwrap()));
        for ip in ["8.8.8.8", "100.64.0.1", "2001:db8::1"] {
            assert!(!is_local(ip.parse().unwrap()), "{ip}");
        }
    }
}
