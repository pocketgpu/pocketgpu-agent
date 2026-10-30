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
