//! The owner's requests on `pgpu/control/1` (Shell, checked before these run): make, list
//! and stop shares.

use std::sync::Arc;

use pocketgpu_proto::msg::control::Response;
use pocketgpu_proto::share::{MAX_OPEN_SHARES, NewShare};
use pocketgpu_proto::{Error, ErrorCode};

use super::{chat, end, info, store, target};
use crate::audit::{self, Event};
use crate::daemon::auth::Peer;
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

fn internal(e: impl std::fmt::Display) -> Response {
    Response::Error(Error::new(ErrorCode::Internal, e.to_string()))
}

pub async fn create(agent: &Arc<Agent>, peer: &Peer, new: NewShare) -> Response {
    if let Err(why) = new.check() {
        return Response::Error(Error::new(ErrorCode::Invalid, why));
    }
    if super::link_relays(agent).is_empty() {
        return Response::Error(Error::new(
            ErrorCode::Invalid,
            "this computer has no relay for browsers to reach it; set one with: pgpu relay set <url>",
        ));
    }
    match store::count_open(&agent.db()) {
        Ok(n) if n >= MAX_OPEN_SHARES => {
            return Response::Error(Error::new(
                ErrorCode::Invalid,
                format!("this computer already shares {n} models; stop one first"),
            ));
        }
        Ok(_) => {}
        Err(e) => return internal(e),
    }
    if !chat::model_available(agent, new.server, &new.model).await {
        return Response::Error(Error::new(
            ErrorCode::NotFound,
            format!("{} isn't available on this computer right now", new.model),
        ));
    }
    let mut secret = [0u8; 32];
    if let Err(e) = getrandom::fill(&mut secret) {
        return internal(e);
    }
    let now = now_ms();
    let db = agent.db();
    let share = match store::insert(&db, &new, secret, &peer.key.to_string(), now) {
        Ok(s) => s,
        Err(e) => return internal(e),
    };
    let _ = audit::record(
        &db,
        &peer.key.to_string(),
        Event::Share,
        &target(&share),
        "made",
    );
    drop(db);
    Response::ShareMade(info(agent, &share, now))
}

pub fn list(agent: &Agent) -> Response {
    let now = now_ms();
    // Read first: `info` locks the database too, so the list's lock must be gone by then.
    let all = store::list(&agent.db());
    match all {
        Ok(all) => Response::Shares(all.iter().map(|s| info(agent, s, now)).collect()),
        Err(e) => internal(e),
    }
}

pub fn stop(agent: &Agent, peer: &Peer, share: u64) -> Response {
    match store::get(&agent.db(), share) {
        Ok(Some(_)) => {}
        Ok(None) => return Response::Error(Error::new(ErrorCode::NotFound, "no such share")),
        Err(e) => return internal(e),
    }
    end(agent, share, &peer.key.to_string(), "stopped");
    list(agent)
}
