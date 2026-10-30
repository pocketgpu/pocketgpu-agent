//! Control requests for actions and processes. Permissions are already checked; every
//! state-changing request writes one audit entry with its result.

use std::sync::Arc;

use pocketgpu_proto::msg::control::{Action, NewAction, Response};
use pocketgpu_proto::{Error, ErrorCode};

use super::store::{self, StoreError};
use super::{runner, validate};
use crate::audit::{self, Event};
use crate::daemon::auth::Peer;
use crate::daemon::processes;
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

fn audit(agent: &Agent, peer: &Peer, event: Event, target: &str, result: &str) {
    if let Err(e) = audit::record(&agent.db(), &peer.key.to_string(), event, target, result) {
        tracing::warn!("could not write audit entry: {e}");
    }
}

fn internal(e: impl std::fmt::Display) -> Response {
    Response::Error(Error::new(ErrorCode::Internal, e.to_string()))
}

fn store_error(e: StoreError) -> Error {
    match e {
        StoreError::Invalid(i) => Error::new(ErrorCode::Invalid, i.to_string()),
        StoreError::NotFound(_) => Error::new(ErrorCode::NotFound, e.to_string()),
        StoreError::Db(e) => Error::new(ErrorCode::Internal, e.to_string()),
    }
}

/// `"#3 Restart vLLM"`: the action's id and name, never its command.
fn label(a: &Action) -> String {
    format!("#{} {}", a.id, a.name)
}

pub fn list(agent: &Agent) -> Response {
    let listed = store::list(&agent.db());
    match listed {
        Ok(a) => Response::Actions(a),
        Err(e) => internal(e),
    }
}

pub fn runs(agent: &Agent, limit: u32) -> Response {
    let runs = store::runs(&agent.db(), limit);
    match runs {
        Ok(r) => Response::Runs(r),
        Err(e) => internal(e),
    }
}

pub fn create(agent: &Agent, peer: &Peer, new: &NewAction) -> Response {
    let res = validate(new)
        .map_err(StoreError::from)
        .and_then(|v| store::create(&agent.db(), &v));
    match res {
        Ok(a) => {
            audit(agent, peer, Event::ActionCreate, &label(&a), "ok");
            Response::Action(a)
        }
        Err(e) => {
            let e = store_error(e);
            audit(
                agent,
                peer,
                Event::ActionCreate,
                new.name.trim(),
                &e.message,
            );
            Response::Error(e)
        }
    }
}

pub fn edit(agent: &Agent, peer: &Peer, id: u64, new: &NewAction) -> Response {
    let res = validate(new)
        .map_err(StoreError::from)
        .and_then(|v| store::edit(&agent.db(), id, &v));
    match res {
        Ok(a) => {
            audit(agent, peer, Event::ActionEdit, &label(&a), "ok");
            Response::Action(a)
        }
        Err(e) => {
            let e = store_error(e);
            audit(
                agent,
                peer,
                Event::ActionEdit,
                &format!("#{id}"),
                &e.message,
            );
            Response::Error(e)
        }
    }
}

pub fn delete(agent: &Agent, peer: &Peer, id: u64) -> Response {
    let name = store::get(&agent.db(), id)
        .ok()
        .flatten()
        .map(|a| label(&a));
    let target = name.unwrap_or_else(|| format!("#{id}"));
    let deleted = store::delete(&agent.db(), id);
    match deleted {
        Ok(()) => {
            audit(agent, peer, Event::ActionDelete, &target, "ok");
            Response::Ok
        }
        Err(e) => {
            let e = store_error(e);
            audit(agent, peer, Event::ActionDelete, &target, &e.message);
            Response::Error(e)
        }
    }
}

pub async fn run(agent: &Arc<Agent>, peer: &Peer, id: u64) -> Response {
    // Bind first: a guard in the match scrutinee would hold the database lock across the
    // arms, and the audit call below needs it.
    let found = store::get(&agent.db(), id);
    let action = match found {
        Ok(Some(a)) => a,
        Ok(None) => {
            let e = Error::new(ErrorCode::NotFound, format!("no action with id {id}"));
            audit(agent, peer, Event::ActionRun, &format!("#{id}"), &e.message);
            return Response::Error(e);
        }
        Err(e) => return internal(e),
    };
    let Some(_guard) = agent.running.start(id) else {
        let e = Error::new(
            ErrorCode::Busy,
            format!("{} is already running", action.name),
        );
        audit(agent, peer, Event::ActionRun, &label(&action), &e.message);
        return Response::Error(e);
    };
    let device = peer.key.to_string();
    let started = store::start_run(&agent.db(), id, Some(&device), now_ms());
    let run_id = match started {
        Ok(r) => r,
        Err(e) => return internal(e),
    };
    tracing::info!(action = %action.name, "running action");
    match runner::run(&action).await {
        Ok(out) => {
            let _ = store::end_run(&agent.db(), run_id, now_ms(), out.exit_code, &out.output);
            let result = match (out.timed_out, out.exit_code) {
                (true, _) => "timed out".to_string(),
                (false, Some(code)) => format!("exit {code}"),
                (false, None) => "killed by a signal".to_string(),
            };
            audit(agent, peer, Event::ActionRun, &label(&action), &result);
            Response::ActionResult {
                exit_code: out.exit_code,
                output: out.output,
                duration_ms: out.duration.as_millis() as u64,
                timed_out: out.timed_out,
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = store::end_run(&agent.db(), run_id, now_ms(), None, &msg);
            audit(agent, peer, Event::ActionRun, &label(&action), &msg);
            internal(msg)
        }
    }
}

pub fn kill(agent: &Arc<Agent>, peer: &Peer, pid: u32) -> Response {
    let target = format!("pid {pid}");
    match processes::kill(agent, pid) {
        Ok(_command) => {
            audit(agent, peer, Event::Kill, &target, "sigterm sent");
            Response::Ok
        }
        Err(e) => {
            audit(agent, peer, Event::Kill, &target, &e.message);
            Response::Error(e)
        }
    }
}
