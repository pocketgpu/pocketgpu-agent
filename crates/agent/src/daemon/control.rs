//! `pgpu/control/1`: one request and one reply per bidirectional stream.

use std::sync::Arc;

use iroh::endpoint::Connection;
use pocketgpu_proto::msg::control::{Request, Response, Status};
use pocketgpu_proto::msg::jobs::JobState;
use pocketgpu_proto::{Error, ErrorCode, MachineInfo, Permission, read_frame, write_frame};

use super::auth::{Peer, PeerProtocol};
use super::state::Agent;
use crate::actions::handler as actions;
use crate::collect::system::os_name;

#[derive(Debug, Clone, Copy)]
pub struct Control;

impl PeerProtocol for Control {
    async fn serve(&self, agent: Arc<Agent>, peer: Peer, conn: Connection) -> anyhow::Result<()> {
        loop {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let agent = agent.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                let reply = match read_frame::<_, Request>(&mut recv).await {
                    Ok(Some(req)) => handle(&agent, &peer, req).await,
                    Ok(None) => return,
                    Err(e) => Response::Error(Error::new(ErrorCode::Invalid, e.to_string())),
                };
                if write_frame(&mut send, &reply).await.is_ok() {
                    let _ = send.finish();
                }
            });
        }
    }
}

pub async fn handle(agent: &Arc<Agent>, peer: &Peer, req: Request) -> Response {
    if let Err(e) = agent.require(peer, req.min_permission(), &describe(&req)) {
        return Response::Error(e);
    }
    // A saved command runs anything, so writing one from a phone is terminal access too.
    if matches!(req, Request::CreateAction(_) | Request::EditAction { .. })
        && !agent.config.read().expect("config lock poisoned").shell
    {
        return Response::Error(Error::new(
            ErrorCode::Forbidden,
            "terminal access is off on this machine, so actions can't be created or edited \
             from a phone. Turn it on there with: pgpu shell on",
        ));
    }
    match req {
        Request::Hello { .. } => Response::MachineInfo(machine_info(agent, peer)),
        Request::Status { .. } => {
            let running = crate::jobs::store::list(&agent.db(), 10, None)
                .ok()
                .and_then(|jobs| jobs.into_iter().find(|j| j.state == JobState::Running));
            Response::Status(Box::new(Status {
                info: machine_info(agent, peer),
                sample: agent.sampler.latest(),
                running,
                games: crate::games::running(agent).await,
            }))
        }
        Request::GetAlertSettings => Response::AlertSettings(crate::alerts::settings::get(agent)),
        Request::SetAlertSettings { rules } => match crate::alerts::settings::set(agent, &rules) {
            Ok(rules) => {
                let _ = crate::audit::record(
                    &agent.db(),
                    &peer.key.to_string(),
                    crate::audit::Event::AlertSettings,
                    "alerts",
                    "ok",
                );
                Response::AlertSettings(rules)
            }
            Err(e) => Response::Error(Error::new(ErrorCode::Invalid, e.to_string())),
        },
        Request::GetHistory { range, gpu } => {
            let now = crate::store::db::now_ms();
            let points = super::history::query(&agent.db(), range, gpu, now);
            match points {
                Ok(p) => Response::History(p),
                Err(e) => Response::Error(Error::new(ErrorCode::Internal, e.to_string())),
            }
        }
        Request::GetRecent => {
            let mut samples = agent.sampler.window(0, u64::MAX);
            for s in &mut samples {
                s.disks.clear();
            }
            Response::Recent(samples)
        }
        Request::SetPushTicket { ticket } => {
            // Tickets are 128-bit hex from the push server; anything else is refused.
            if ticket.len() != 32 || !ticket.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Response::Error(Error::new(ErrorCode::Invalid, "not a push ticket"));
            }
            let saved =
                crate::store::devices::set_push_ticket(&agent.db(), &peer.key.to_string(), &ticket);
            match saved {
                Ok(true) => Response::Ok,
                Ok(false) => Response::Error(Error::new(ErrorCode::NotFound, "not paired")),
                Err(e) => Response::Error(Error::new(ErrorCode::Internal, e.to_string())),
            }
        }
        Request::ListProcesses => Response::Processes(super::processes::list(agent)),
        Request::KillProcess { pid } => actions::kill(agent, peer, pid),
        Request::ListActions => actions::list(agent),
        Request::ListRuns { limit } => actions::runs(agent, limit),
        Request::ListGames => Response::Games(crate::games::running(agent).await),
        Request::RunAction { id } => actions::run(agent, peer, id).await,
        Request::CreateAction(a) => actions::create(agent, peer, &a),
        Request::EditAction { id, action } => actions::edit(agent, peer, id, &action),
        Request::DeleteAction { id } => actions::delete(agent, peer, id),
        Request::Unpair => match super::devices::unpair(agent, peer.key) {
            Ok(_) => Response::Ok,
            Err(e) => Response::Error(Error::new(ErrorCode::Internal, e.to_string())),
        },
    }
}

pub fn machine_info(agent: &Agent, peer: &Peer) -> MachineInfo {
    MachineInfo {
        name: agent.name(),
        os: os_name(),
        agent_version: env!("CARGO_PKG_VERSION").into(),
        gpus: agent.sampler.gpus().info(),
        permission: agent.permission_of(&peer.key).unwrap_or(Permission::View),
    }
}

/// Short text for the audit log; never includes command text or secrets.
fn describe(req: &Request) -> String {
    match req {
        Request::Hello { .. } => "Hello".into(),
        Request::Status { .. } => "Status".into(),
        Request::GetHistory { .. } => "GetHistory".into(),
        Request::GetRecent => "GetRecent".into(),
        Request::ListProcesses => "ListProcesses".into(),
        Request::KillProcess { pid } => format!("KillProcess {pid}"),
        Request::ListActions => "ListActions".into(),
        Request::RunAction { id } => format!("RunAction {id}"),
        Request::CreateAction(a) => format!("CreateAction {}", a.name),
        Request::EditAction { id, .. } => format!("EditAction {id}"),
        Request::DeleteAction { id } => format!("DeleteAction {id}"),
        Request::GetAlertSettings => "GetAlertSettings".into(),
        Request::SetAlertSettings { .. } => "SetAlertSettings".into(),
        Request::SetPushTicket { .. } => "SetPushTicket".into(),
        Request::Unpair => "Unpair".into(),
        Request::ListRuns { .. } => "ListRuns".into(),
        Request::ListGames => "ListGames".into(),
    }
}
