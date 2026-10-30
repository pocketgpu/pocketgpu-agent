//! Pairing a phone: the code slot, the `pgpu/pair/1` handler, and the `pgpu pair` session.

pub mod code;
pub mod handler;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use iroh::{EndpointAddr, PublicKey};
use pocketgpu_proto::local::{PairAsk, PairCodeInfo, Request, Response};
use pocketgpu_proto::pairing::{self, QrPayload};
use pocketgpu_proto::{Permission, write_frame};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::audit::{self, Event};
use crate::daemon::endpoint::{self, Relays};
use crate::daemon::state::Agent;
use crate::store::devices;
use code::{ActiveCode, Ask, Refusal};

/// How long the person at the machine has to answer once a phone proved the code.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the rendezvous endpoint outlives its session.
const RENDEZVOUS_GRACE: Duration = Duration::from_secs(10);

/// The one pairing code that may be active, and how the agent's endpoints reach relays.
#[derive(Debug, Default)]
pub struct Pairing {
    slot: Mutex<Option<Slot>>,
    relays: OnceLock<Relays>,
}

#[derive(Debug)]
struct Slot {
    session: u64,
    code: ActiveCode,
    rendezvous: Option<EndpointAddr>,
}

impl Pairing {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Slot>> {
        self.slot.lock().expect("pairing lock poisoned")
    }

    /// Remembers the relay setup of the main endpoint, for rendezvous endpoints.
    pub fn set_relays(&self, relays: Relays) {
        let _ = self.relays.set(relays);
    }

    pub fn relays(&self) -> Relays {
        self.relays.get().cloned().unwrap_or(Relays::FromConfig)
    }

    /// Whether unknown keys may try a code right now.
    pub fn is_open(&self) -> bool {
        self.slot()
            .as_ref()
            .is_some_and(|s| s.code.is_open(Instant::now()))
    }

    /// Checks a phone's proof. Too many wrong proofs end the code.
    pub fn check(
        &self,
        endpoint: &PublicKey,
        device: &PublicKey,
        proof: &[u8; 32],
    ) -> Result<mpsc::Sender<Ask>, Refusal> {
        let mut slot = self.slot();
        let Some(s) = slot.as_mut() else {
            return Err(Refusal::NoCode);
        };
        let result = s.code.check(Instant::now(), endpoint, device, proof);
        if matches!(result, Err(Refusal::Burned)) {
            *slot = None;
        }
        result
    }

    /// The rendezvous endpoint of the active code, for tests that can't look it up.
    pub fn rendezvous_addr(&self) -> Option<EndpointAddr> {
        self.slot().as_ref().and_then(|s| s.rendezvous.clone())
    }

    fn install(&self, session: u64, code: ActiveCode) -> Result<()> {
        let mut slot = self.slot();
        if slot
            .as_ref()
            .is_some_and(|s| s.code.expires_at > Instant::now())
        {
            bail!("another `pgpu pair` is already running");
        }
        *slot = Some(Slot {
            session,
            code,
            rendezvous: None,
        });
        Ok(())
    }

    fn set_rendezvous(&self, session: u64, addr: EndpointAddr) {
        if let Some(s) = self.slot().as_mut().filter(|s| s.session == session) {
            s.rendezvous = Some(addr);
        }
    }

    fn clear(&self, session: u64) {
        let mut slot = self.slot();
        if slot.as_ref().is_some_and(|s| s.session == session) {
            *slot = None;
        }
    }
}

/// Clears the session's code however the session ends.
struct ClearOnDrop<'a>(&'a Pairing, u64);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.clear(self.1);
    }
}

/// Runs one `pgpu pair` over the local socket: shows a code, asks about the phone that
/// proves it, and pairs it. Ends with `Paired` or `PairEnded`, or silently when the command
/// line goes away.
pub async fn session<R, W>(agent: Arc<Agent>, reader: R, mut w: W) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let (asks_tx, mut asks) = mpsc::channel(1);
    let active = ActiveCode::generate(asks_tx)?;
    let code = active.code;
    let expires_at = active.expires_at;
    let id = next_session_id();
    if let Err(e) = agent.pairing.install(id, active) {
        write_frame(&mut w, &Response::Error(e.to_string())).await?;
        return Ok(());
    }
    let _clear = ClearOnDrop(&agent.pairing, id);

    let rendezvous = match endpoint::rendezvous(&agent, &code).await {
        Ok(router) => {
            agent.pairing.set_rendezvous(id, router.endpoint().addr());
            Some(router)
        }
        Err(e) => {
            tracing::warn!("typed codes won't work for this code: {e:#}");
            None
        }
    };

    let relays = agent
        .config
        .read()
        .expect("config lock poisoned")
        .relays
        .clone();
    let qr = QrPayload {
        agent_id: agent.identity.agent_id().to_string(),
        code,
        relays,
    };
    let info = PairCodeInfo {
        code: code.to_string(),
        qr: qr.to_uri(),
        expires_in_s: expires_at
            .saturating_duration_since(Instant::now())
            .as_secs() as u32,
        agent_fingerprint: agent.identity.fingerprint(),
    };
    write_frame(&mut w, &Response::PairCode(info)).await?;

    let mut from_cli = crate::ipc::forward_frames(reader);
    let expired = tokio::time::sleep_until(expires_at.into());
    tokio::pin!(expired);
    let outcome = loop {
        tokio::select! {
            _ = &mut expired => {
                break Some(Response::PairEnded("The code expired. Run `pgpu pair` again.".into()));
            }
            ask = asks.recv() => match ask {
                None => {
                    break Some(Response::PairEnded(
                        "Too many wrong codes were tried. Run `pgpu pair` again.".into(),
                    ));
                }
                Some(ask) => break decide(&agent, ask, &mut w, &mut from_cli).await?,
            },
            msg = from_cli.recv() => {
                if msg.is_none() {
                    break None;
                }
            }
        }
    };
    if let Some(r) = outcome {
        write_frame(&mut w, &r).await?;
    }
    if let Some(router) = rendezvous {
        // Let a phone on the rendezvous endpoint read its answer first. No new phone can
        // start: the code is gone once this session returns.
        tokio::spawn(async move {
            tokio::time::sleep(RENDEZVOUS_GRACE).await;
            router.shutdown().await.ok();
        });
    }
    Ok(())
}

/// Asks the command line about a phone that proved the code, and pairs it if approved.
async fn decide<W: AsyncWrite + Unpin>(
    agent: &Agent,
    ask: Ask,
    w: &mut W,
    from_cli: &mut mpsc::Receiver<Request>,
) -> Result<Option<Response>> {
    let device_id = ask.device.to_string();
    let question = PairAsk {
        device_name: clean_name(&ask.device_name),
        platform: ask.platform,
        device_fingerprint: pairing::key_groups(&device_id),
    };
    write_frame(w, &Response::PairAsk(question.clone())).await?;

    let answer = tokio::time::timeout(APPROVAL_TIMEOUT, async {
        loop {
            match from_cli.recv().await {
                Some(Request::PairDecision { permission }) => return Some(permission),
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await;
    let permission: Option<Permission> = match answer {
        Ok(Some(p)) => p,
        Ok(None) => {
            let _ = ask.decision.send(None);
            return Ok(None);
        }
        Err(_) => {
            let _ = ask.decision.send(None);
            return Ok(Some(Response::PairEnded(
                "No answer in time; the phone was not paired.".into(),
            )));
        }
    };

    let Some(permission) = permission else {
        let _ = ask.decision.send(None);
        return Ok(Some(Response::PairEnded(format!(
            "Denied \"{}\".",
            question.device_name
        ))));
    };
    {
        let db = agent.db();
        devices::upsert(
            &db,
            &device_id,
            &question.device_name,
            ask.platform,
            permission,
            ask.push_ticket.as_deref(),
        )?;
        audit::record(
            &db,
            &device_id,
            Event::Pair,
            &question.device_name,
            permission.as_str(),
        )?;
    }
    if ask.decision.send(Some(permission)).is_err() {
        tracing::warn!("the phone left before hearing it was paired");
    }
    Ok(Some(Response::Paired {
        device_name: question.device_name,
        permission,
    }))
}

/// The name a phone gave itself, as shown in prompts and lists: no control characters (which
/// could redraw the terminal prompt) or quotes, at most 40 characters.
fn clean_name(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '"' | '`' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(40)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "Phone".into()
    } else if name.eq_ignore_ascii_case("local") {
        // `local` is how the audit log names the machine's own command line.
        "local phone".into()
    } else {
        name.into()
    }
}

fn next_session_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod name_tests {
    use super::clean_name;

    #[test]
    fn names_cannot_redraw_the_prompt() {
        assert_eq!(clean_name("Pixel 8"), "Pixel 8");
        assert_eq!(clean_name("x\r\x1b[2K\"iPhone\""), "x[2KiPhone");
        assert_eq!(clean_name("evil\u{202e}gnp"), "evilgnp");
        assert_eq!(clean_name("  \n "), "Phone");
        assert_eq!(clean_name(&"a".repeat(80)).len(), 40);
        assert_eq!(clean_name("LOCAL"), "local phone");
    }
}
