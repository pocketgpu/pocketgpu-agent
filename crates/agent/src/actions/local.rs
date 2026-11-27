//! Action requests from the local command line, which is trusted as the agent user.
//! Changes are audited with the device `local`.

use pocketgpu_proto::local::{ActionEdit, Response};
use pocketgpu_proto::msg::control::NewAction;

use super::store::{self, StoreError};
use super::validate;
use crate::audit::{self, Event};
use crate::daemon::state::Agent;

fn audit(agent: &Agent, event: Event, target: &str, result: &str) {
    if let Err(e) = audit::record(&agent.db(), "local", event, target, result) {
        tracing::warn!("could not write audit entry: {e}");
    }
}

fn error(e: impl std::fmt::Display) -> Response {
    Response::Error(e.to_string())
}

pub fn list(agent: &Agent) -> Response {
    let listed = store::list(&agent.db());
    match listed {
        Ok(a) => Response::Actions(a),
        Err(e) => error(format!("{e:#}")),
    }
}

pub fn add(agent: &Agent, new: &NewAction) -> Response {
    let res = validate(new)
        .map_err(StoreError::from)
        .and_then(|v| store::create(&agent.db(), &v));
    match res {
        Ok(a) => {
            audit(
                agent,
                Event::ActionCreate,
                &format!("#{} {}", a.id, a.name),
                "ok",
            );
            Response::Action(a)
        }
        Err(e) => error(e),
    }
}

pub fn edit(agent: &Agent, edit: &ActionEdit) -> Response {
    let found = store::find(&agent.db(), &edit.action);
    let current = match found {
        Ok(Some(a)) => a,
        Ok(None) => return error(format!("no action {:?}", edit.action)),
        Err(e) => return error(format!("{e:#}")),
    };
    let merged = NewAction {
        name: edit.name.clone().unwrap_or(current.name),
        command: edit.command.clone().unwrap_or(current.command),
        icon: edit.icon.clone().unwrap_or(current.icon),
        risky: edit.risky.unwrap_or(current.risky),
        timeout_s: Some(edit.timeout_s.unwrap_or(current.timeout_s)),
    };
    let res = validate(&merged)
        .map_err(StoreError::from)
        .and_then(|v| store::edit(&agent.db(), current.id, &v));
    match res {
        Ok(a) => {
            audit(
                agent,
                Event::ActionEdit,
                &format!("#{} {}", a.id, a.name),
                "ok",
            );
            Response::Action(a)
        }
        Err(e) => error(e),
    }
}

pub fn remove(agent: &Agent, action: &str) -> Response {
    let found = store::find(&agent.db(), action);
    let a = match found {
        Ok(Some(a)) => a,
        Ok(None) => return error(format!("no action {action:?}")),
        Err(e) => return error(format!("{e:#}")),
    };
    let deleted = store::delete(&agent.db(), a.id);
    match deleted {
        Ok(()) => {
            audit(
                agent,
                Event::ActionDelete,
                &format!("#{} {}", a.id, a.name),
                "ok",
            );
            Response::Ok
        }
        Err(e) => error(e),
    }
}

/// Runs the action with this name or number for an AI assistant (F32 `run_action`), whose
/// level the caller has checked. Audited as `local`, the target naming the assistant.
pub async fn run(agent: &Agent, action: &str, assistant: &str) -> Response {
    let found = store::find(&agent.db(), action);
    let action = match found {
        Ok(Some(a)) => a,
        Ok(None) => return error(format!("no action named {action}; see pgpu actions")),
        Err(e) => return error(format!("{e:#}")),
    };
    let target = format!("#{} {} · {assistant}", action.id, action.name);
    let Some(_guard) = agent.running.start(action.id) else {
        return error(format!("{} is already running", action.name));
    };
    let started = super::store::start_run(&agent.db(), action.id, None, crate::store::db::now_ms());
    let run_id = match started {
        Ok(r) => r,
        Err(e) => return error(format!("{e:#}")),
    };
    let now = crate::store::db::now_ms;
    match super::runner::run(&action).await {
        Ok(out) => {
            let _ = store::end_run(&agent.db(), run_id, now(), out.exit_code, &out.output);
            let result = match (out.timed_out, out.exit_code) {
                (true, _) => "timed out".to_string(),
                (false, Some(code)) => format!("exit {code}"),
                (false, None) => "killed by a signal".to_string(),
            };
            audit(agent, Event::ActionRun, &target, &result);
            Response::ActionRan {
                exit_code: out.exit_code,
                output: out.output,
                duration_ms: out.duration.as_millis() as u64,
                timed_out: out.timed_out,
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = store::end_run(&agent.db(), run_id, now(), None, &msg);
            audit(agent, Event::ActionRun, &target, &msg);
            error(msg)
        }
    }
}
