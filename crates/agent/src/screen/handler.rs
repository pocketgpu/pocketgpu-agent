//! `pgpu/screen/1` (F45): `Info` gets one reply, `Peek` one picture. `Watch` sends a picture
//! each time the screen changes, at most `fps` a second, and `Same` every few seconds while
//! it doesn't, until the device closes the stream, loses Full control, or someone here stops
//! it or turns the screen off. Permissions are checked again every second.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, SendStream};
use pocketgpu_proto::screen::{Frame, MAX_FPS, Request, Response, width_for};
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};

use super::capture::{self, Shot};
use crate::daemon::auth::{Peer, PeerProtocol, until_closed};
use crate::daemon::state::Agent;

/// A `Same` this often while nothing changes, so a closed stream is noticed.
const SAME_EVERY: Duration = Duration::from_secs(2);
/// Permissions and the setting are looked at again this often.
const CHECK_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
pub struct Screen;

impl PeerProtocol for Screen {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (send, mut recv) = conn.accept_bi().await?;
            let (agent, peer, closed) = (agent.clone(), peer.clone(), conn.clone());
            tokio::spawn(until_closed(closed, async move {
                match read_frame::<_, Request>(&mut recv).await {
                    Ok(Some(req)) => handle(&agent, &peer, req, send).await,
                    Ok(None) => {}
                    Err(e) => {
                        let err = Error::new(ErrorCode::Invalid, e.to_string());
                        reply(send, Response::Error(err)).await;
                    }
                }
            }));
        }
    }
}

async fn reply(mut send: SendStream, r: Response) {
    let _ = write_frame(&mut send, &r).await;
    let _ = send.finish();
}

fn now_ms() -> u64 {
    crate::store::db::now_ms().max(0) as u64
}

async fn grab(display: u32) -> Result<Shot, Error> {
    tokio::task::spawn_blocking(move || capture::grab(display))
        .await
        .map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?
        .map_err(|e| Error::new(ErrorCode::Busy, e))
}

async fn picture(shot: Shot, display: u32, max_width: u32) -> Result<Frame, Error> {
    tokio::task::spawn_blocking(move || {
        let width = width_for(max_width, shot.width);
        super::encode::jpeg(&shot, width).map(|(jpeg, width, height)| Frame {
            display,
            width,
            height,
            jpeg,
            at_ms: now_ms(),
        })
    })
    .await
    .map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?
    .map_err(|e| Error::new(ErrorCode::Internal, e))
}

fn allowed(agent: &Agent, peer: &Peer, req: &Request, what: &str) -> Result<(), Error> {
    agent.require(peer, req.min_permission(), what)?;
    if matches!(req, Request::Info) {
        Ok(())
    } else {
        super::check(agent)
    }
}

async fn handle(agent: &Arc<Agent>, peer: &Peer, req: Request, send: SendStream) {
    let what = match req {
        Request::Info => "Screen Info",
        Request::Peek { .. } => "Screen Peek",
        Request::Watch { .. } => "Screen Watch",
    };
    if let Err(e) = allowed(agent, peer, &req, what) {
        return reply(send, Response::Error(e)).await;
    }
    match req {
        Request::Info => {
            let a = agent.clone();
            let info = tokio::task::spawn_blocking(move || super::info(&a))
                .await
                .unwrap_or_else(|_| super::info(agent));
            reply(send, Response::Info(info)).await
        }
        Request::Peek { display, max_width } => {
            super::record_look(agent, &peer.key.to_string(), display);
            let r = match grab(display).await {
                Ok(shot) => picture(shot, display, max_width).await,
                Err(e) => Err(e),
            };
            reply(send, r.map_or_else(Response::Error, Response::Frame)).await
        }
        Request::Watch {
            display,
            max_width,
            fps,
        } => watch(agent, peer, send, display, max_width, fps).await,
    }
}

async fn watch(
    agent: &Arc<Agent>,
    peer: &Peer,
    mut send: SendStream,
    display: u32,
    max_width: u32,
    fps: u8,
) {
    let device = peer.key.to_string();
    let name = crate::store::devices::get(&agent.db(), &device)
        .ok()
        .flatten()
        .map_or_else(|| "A device".to_string(), |d| d.name);
    super::record_look(agent, &device, display);
    let look = agent.screen.start(device, name, display);
    let every = Duration::from_millis(1000 / u64::from(fps.clamp(1, MAX_FPS)));
    let mut last: Option<blake3::Hash> = None;
    let mut sent = Instant::now();
    let mut checked = Instant::now();
    loop {
        let began = Instant::now();
        if look.stop.load(Ordering::Relaxed) {
            return reply(send, Response::Stopped).await;
        }
        if checked.elapsed() >= CHECK_EVERY {
            checked = Instant::now();
            let req = Request::Watch {
                display,
                max_width,
                fps,
            };
            if let Err(e) = allowed(agent, peer, &req, "Screen Watch") {
                let r = if e.code == ErrorCode::Forbidden && !super::enabled(agent) {
                    Response::Stopped
                } else {
                    Response::Error(e)
                };
                return reply(send, r).await;
            }
        }
        let shot = match grab(display).await {
            Ok(s) => s,
            Err(e) => return reply(send, Response::Error(e)).await,
        };
        let hash = blake3::hash(&shot.bgra);
        let msg = if last == Some(hash) {
            if sent.elapsed() < SAME_EVERY {
                None
            } else {
                Some(Response::Same { at_ms: now_ms() })
            }
        } else {
            last = Some(hash);
            match picture(shot, display, max_width).await {
                Ok(f) => Some(Response::Frame(f)),
                Err(e) => return reply(send, Response::Error(e)).await,
            }
        };
        if let Some(m) = msg {
            if write_frame(&mut send, &m).await.is_err() {
                return;
            }
            sent = Instant::now();
        }
        if let Some(rest) = every.checked_sub(began.elapsed()) {
            tokio::time::sleep(rest).await;
        }
    }
}
