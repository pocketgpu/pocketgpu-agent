//! `pgpu/shell/1`: one stream per terminal (F16).
//!
//! Opening needs terminal access turned on at the machine, an agent that isn't root, and a
//! phone with Shell permission. Reattaching works only from the phone that opened the session.

use std::sync::Arc;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use pocketgpu_proto::shell::{ToAgent, ToPhone};
use pocketgpu_proto::{Error, ErrorCode, Permission, read_frame, write_frame};
use tokio::sync::mpsc;

use super::{Event, Session};
use crate::daemon::auth::{Peer, PeerProtocol};
use crate::daemon::root::{RootCheck, check as root_check};
use crate::daemon::state::Agent;

#[derive(Debug, Clone, Copy)]
pub struct Shell;

impl PeerProtocol for Shell {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (send, recv) = conn.accept_bi().await?;
            tokio::spawn(serve_stream(agent.clone(), peer.clone(), send, recv));
        }
    }
}

async fn serve_stream(agent: Arc<Agent>, peer: Peer, mut send: SendStream, mut recv: RecvStream) {
    let first = match read_frame::<_, ToAgent>(&mut recv).await {
        Ok(Some(m)) => m,
        _ => return,
    };
    let what = match &first {
        ToAgent::List => "ListShells",
        ToAgent::Tools(_) => "ListTools",
        _ => "OpenShell",
    };
    if let Err(e) = allowed(&agent, &peer, what) {
        let _ = write_frame(&mut send, &ToPhone::Error(e)).await;
        let _ = send.finish();
        return;
    }
    match first {
        ToAgent::List => {
            let list = agent.shells.list(&peer.key);
            let _ = write_frame(&mut send, &ToPhone::Sessions(list)).await;
            let _ = send.finish();
        }
        ToAgent::Tools(names) => {
            let found = super::tools::installed(&names).await;
            let _ = write_frame(&mut send, &ToPhone::Tools(found)).await;
            let _ = send.finish();
        }
        ToAgent::Open {
            cols,
            rows,
            session,
        } => {
            let opened = match session {
                Some(id) => agent.shells.get(&peer.key, id).ok_or_else(|| {
                    Error::new(ErrorCode::NotFound, "that terminal session has ended")
                }),
                None => agent.shells.open(&agent, peer.key, cols, rows),
            };
            match opened {
                Ok(s) => {
                    s.resize(cols, rows);
                    attached(&agent, &s, session.is_some(), send, recv).await;
                }
                Err(e) => {
                    let _ = write_frame(&mut send, &ToPhone::Error(e)).await;
                    let _ = send.finish();
                }
            }
        }
        _ => {
            let e = Error::new(
                ErrorCode::Invalid,
                "a terminal stream starts with Open or List",
            );
            let _ = write_frame(&mut send, &ToPhone::Error(e)).await;
            let _ = send.finish();
        }
    }
}

/// Terminal access on, not root, and Shell permission (audited when refused).
fn allowed(agent: &Agent, peer: &Peer, what: &str) -> Result<(), Error> {
    if !agent.config.read().expect("config lock poisoned").shell {
        return Err(Error::new(
            ErrorCode::Forbidden,
            "terminal access is off on this machine. Turn it on there with: pgpu shell on",
        ));
    }
    if matches!(root_check(), Ok(RootCheck::RootInContainer)) {
        return Err(Error::new(
            ErrorCode::Forbidden,
            "the agent runs as root in this container, so the terminal stays off",
        ));
    }
    agent.require(peer, Permission::Shell, what)
}

/// Pipes the session to and from the phone until it closes, exits or detaches.
async fn attached(
    agent: &Arc<Agent>,
    session: &Arc<Session>,
    reattached: bool,
    mut send: SendStream,
    recv: RecvStream,
) {
    let (attach, mut events, replay) = session.attach(&agent.shells);
    let opened = ToPhone::Opened {
        session: session.id,
        title: session.title(),
        reattached,
    };
    if write_frame(&mut send, &opened).await.is_err() {
        session.detach(attach);
        return;
    }
    if reattached
        && !replay.is_empty()
        && write_frame(&mut send, &ToPhone::Output(replay))
            .await
            .is_err()
    {
        session.detach(attach);
        return;
    }

    let mut input = read_messages(recv);
    loop {
        tokio::select! {
            event = events.recv() => {
                let msg = match event {
                    Some(Event::Output(b)) => ToPhone::Output(b),
                    Some(Event::Title(t)) => ToPhone::Title(t),
                    Some(Event::Exited(code)) => ToPhone::Exited { code },
                    Some(Event::Closed(e)) => ToPhone::Error(e),
                    // Another stream took over, or the session was closed.
                    None => break,
                };
                let last = matches!(msg, ToPhone::Exited { .. } | ToPhone::Error(_));
                if write_frame(&mut send, &msg).await.is_err() {
                    session.detach(attach);
                    return;
                }
                if last {
                    break;
                }
            }
            msg = input.recv() => match msg {
                Some(ToAgent::Input(bytes)) => session.write(bytes),
                Some(ToAgent::Resize { cols, rows }) => session.resize(cols, rows),
                Some(ToAgent::Close) => {
                    agent.shells.close(agent, session.id, None, "closed");
                    break;
                }
                Some(ToAgent::Open { .. } | ToAgent::List | ToAgent::Tools(_)) => {}
                // The phone went away without closing: keep the session for a reattach.
                None => {
                    session.detach(attach);
                    return;
                }
            },
        }
    }
    let _ = send.finish();
}

/// Reads frames on their own task: `read_frame` can't be cancelled midway in a `select!`.
fn read_messages(mut recv: RecvStream) -> mpsc::Receiver<ToAgent> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Ok(Some(msg)) = read_frame::<_, ToAgent>(&mut recv).await {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    });
    rx
}
