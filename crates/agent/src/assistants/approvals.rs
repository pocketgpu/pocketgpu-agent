//! Held AI assistant actions waiting for the phone (F32, data-model.md). In memory only: a
//! request never outlives the hook that waits for it. Plain rules with the clock passed in;
//! [`super::hold`] does the timers, the push, the device checks and the audit.
//!
//! Safety: the only way to `Answer::Allow` is [`Approvals::answer`] with `allow` (from a
//! Full control phone, checked by the caller) or a remembered phone Allow. Every other end
//! (the clock, no phone, the hook gone, the desk) gives a no-decision or a deny. Only an
//! "Ask me first" Allow is remembered, for the desk prompt that may follow that one run;
//! the next check of the same call forgets it, so each run needs its own Allow.

use std::collections::HashMap;
use std::sync::Mutex;

use pocketgpu_proto::assistants::{
    Answer, ApprovalState, ApprovalView, AskWhy, DETAIL_MAX, ENDED_KEEP_MS, Ended, Kind, OPEN_MAX,
    REMEMBER_ALLOW_MS, WAIT_MS,
};
use tokio::sync::oneshot;

pub use super::answers::{COULDNT_ASK, DENIED, Hash, NO_ANSWER, TOO_LONG, fail_closed, input_hash};

/// Remembered Allows kept per session at most.
const REMEMBER_MAX: usize = 16;

/// A request to open; `kind`, `tool` and `detail_text` come from `kinds.rs`.
#[derive(Debug, Clone)]
pub struct NewRequest {
    pub why: AskWhy,
    pub kind: Kind,
    pub assistant: String,
    pub session: String,
    pub permission_mode: String,
    pub tool: String,
    /// The command for a shell tool, else the input: what the phone shows.
    pub detail_text: String,
    pub input_hash: Hash,
    pub folder: String,
    /// Already masked and cut ([`super::secrets::summary`]).
    pub summary: String,
}

/// A held action's waiting hook: its id, which waiter it is, and where the answer comes.
#[derive(Debug)]
pub struct Held {
    pub id: u64,
    pub waiter: u64,
    pub rx: oneshot::Receiver<Answer>,
    /// It joined an identical held request: no new timer or push.
    pub joined: bool,
}

#[derive(Debug)]
pub enum Opened {
    Held(Held),
    /// Not held (too many open): answered at once, and the end to audit.
    Ended(Answer, Done),
}

/// A request that just ended, for the audit (written once, at the end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub id: u64,
    pub answered_by: Option<String>,
    pub target: String,
    pub state: ApprovalState,
}

/// What the desk did in a session (AskOnPhone only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desk {
    /// `PostToolUse` or `PostToolUseFailure` for this tool and input.
    ToolDone(Hash),
    /// The session's `Stop` or its next prompt.
    TurnOver,
}

#[derive(Debug)]
struct Request {
    view: ApprovalView,
    session: String,
    permission_mode: String,
    input_hash: Hash,
    summary: String,
    answered_by: Option<String>,
    waiters: Vec<(u64, oneshot::Sender<Answer>)>,
    ended_ms: Option<i64>,
}

impl Request {
    fn done(&self) -> Done {
        let kind = match self.view.kind {
            Kind::OwnWords => "own-words",
            Kind::Other => "other",
            Kind::Unknown => "unknown",
            k => k.config_name().unwrap_or("other"),
        };
        Done {
            id: self.view.id,
            answered_by: self.answered_by.clone(),
            target: format!(
                "{} · {} · {kind} · {}",
                self.view.assistant, self.view.session_short, self.summary
            ),
            state: self.view.state,
        }
    }

    /// What the hook hears when the request ends without a phone answer.
    fn no_answer(&self, why: Ended) -> Answer {
        match (self.view.why, why) {
            (AskWhy::AskFirst, Ended::TimedOut) => Answer::Deny {
                message: NO_ANSWER.into(),
            },
            (AskWhy::AskFirst, why) => {
                fail_closed(&self.view.assistant, &self.permission_mode, why)
            }
            (AskWhy::AskOnPhone, why) => Answer::NoDecision { why },
        }
    }

    /// Ends it and tells every waiting hook.
    fn finish(&mut self, state: ApprovalState, answer: Answer, now: i64) -> Done {
        self.view.state = state;
        self.ended_ms = Some(now);
        for (_, tx) in self.waiters.drain(..) {
            let _ = tx.send(answer.clone());
        }
        self.done()
    }
}

#[derive(Debug, Default)]
struct Inner {
    requests: HashMap<u64, Request>,
    /// Phone Allows per (assistant, session): the hash and when.
    remembered: HashMap<(String, String), Vec<(Hash, i64)>>,
    next_waiter: u64,
}

#[derive(Debug, Default)]
pub struct Approvals {
    inner: Mutex<Inner>,
}

impl Approvals {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("approvals lock poisoned")
    }

    /// Holds an action, joining an identical held one (same assistant, session, hook and
    /// hash). With `OPEN_MAX` open already it ends at once as no phone reached.
    pub fn open(&self, new: NewRequest, now: i64) -> Opened {
        let mut inner = self.lock();
        inner.forget_old(now);
        if new.why == AskWhy::AskFirst {
            inner.forget(&new.assistant, &new.session, &new.input_hash);
        }
        inner.next_waiter += 1;
        let waiter = inner.next_waiter;
        let (tx, rx) = oneshot::channel();
        if let Some(r) = inner.requests.values_mut().find(|r| {
            r.view.state.is_open()
                && r.view.why == new.why
                && r.view.assistant == new.assistant
                && r.session == new.session
                && r.input_hash == new.input_hash
        }) {
            r.waiters.push((waiter, tx));
            return Opened::Held(Held {
                id: r.view.id,
                waiter,
                rx,
                joined: true,
            });
        }
        let open = inner
            .requests
            .values()
            .filter(|r| r.view.state.is_open())
            .count();
        let id = inner.new_id();
        let truncated = new.detail_text.len() > DETAIL_MAX;
        let mut detail = new.detail_text;
        if truncated {
            let mut cut = DETAIL_MAX;
            while !detail.is_char_boundary(cut) {
                cut -= 1;
            }
            detail.truncate(cut);
        }
        let mut req = Request {
            view: ApprovalView {
                id,
                opened_ms: now,
                ends_ms: now + WAIT_MS,
                assistant: new.assistant,
                session_short: new.session.chars().take(8).collect(),
                why: new.why,
                kind: new.kind,
                tool: new.tool,
                detail,
                truncated,
                folder: new.folder,
                state: ApprovalState::Waiting,
                answered_by_name: None,
                risky: new.why == AskWhy::AskFirst || new.kind != Kind::Other,
            },
            session: new.session,
            permission_mode: new.permission_mode,
            input_hash: new.input_hash,
            summary: new.summary,
            answered_by: None,
            waiters: Vec::new(),
            ended_ms: None,
        };
        if open >= OPEN_MAX {
            let answer = req.no_answer(Ended::NoPhone);
            let done = req.finish(ApprovalState::NoPhone, answer.clone(), now);
            inner.requests.insert(id, req);
            return Opened::Ended(answer, done);
        }
        req.waiters.push((waiter, tx));
        inner.requests.insert(id, req);
        Opened::Held(Held {
            id,
            waiter,
            rx,
            joined: false,
        })
    }

    /// A phone's Allow for this call in this session in the last minute, used up if so.
    pub fn take_remembered(&self, assistant: &str, session: &str, hash: &Hash, now: i64) -> bool {
        let mut inner = self.lock();
        inner.forget_old(now);
        let key = (assistant.to_string(), session.to_string());
        let Some(list) = inner.remembered.get_mut(&key) else {
            return false;
        };
        let Some(at) = list.iter().position(|(h, _)| h == hash) else {
            return false;
        };
        list.remove(at);
        true
    }

    /// A new run of this call: an earlier phone Allow no longer counts for it.
    pub fn forget(&self, assistant: &str, session: &str, hash: &Hash) {
        self.lock().forget(assistant, session, hash);
    }

    /// A phone's answer. The caller has checked the device (Full control, paired for good).
    /// The first answer wins; `id` and `opened_ms` must match an open request.
    #[allow(clippy::too_many_arguments)]
    pub fn answer(
        &self,
        id: u64,
        opened_ms: i64,
        device: &str,
        device_name: &str,
        allow: bool,
        now: i64,
    ) -> Result<(ApprovalView, Done), String> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        inner.forget_old(now);
        let Some(req) = inner.requests.get_mut(&id) else {
            return Err("no longer open".into());
        };
        if req.view.opened_ms != opened_ms {
            return Err("no longer open".into());
        }
        if !req.view.state.is_open() {
            let by = req.view.answered_by_name.clone().unwrap_or_default();
            return Err(match req.view.state {
                ApprovalState::Allowed => format!("already allowed on {by}"),
                ApprovalState::Denied => format!("already denied on {by}"),
                ApprovalState::AnsweredAtComputer => "already answered at the computer".into(),
                _ => "no longer open".into(),
            });
        }
        if allow && req.view.truncated {
            return Err(TOO_LONG.into());
        }
        req.answered_by = Some(device.to_string());
        req.view.answered_by_name = Some(device_name.to_string());
        let done = if allow {
            req.finish(ApprovalState::Allowed, Answer::Allow, now)
        } else {
            let deny = Answer::Deny {
                message: DENIED.into(),
            };
            req.finish(ApprovalState::Denied, deny, now)
        };
        let view = req.view.clone();
        // Only "Ask me first": an "Ask on my phone" Allow already was the desk's answer.
        if allow && view.why == AskWhy::AskFirst {
            let key = (view.assistant.clone(), req.session.clone());
            let hash = req.input_hash;
            let list = inner.remembered.entry(key).or_default();
            list.push((hash, now));
            if list.len() > REMEMBER_MAX {
                list.remove(0);
            }
        }
        Ok((view, done))
    }

    /// Ends an open request without a phone answer: the clock, no phone reached. Nothing
    /// when it already ended.
    pub fn end(&self, id: u64, why: Ended, now: i64) -> Option<Done> {
        let mut inner = self.lock();
        let req = inner.requests.get_mut(&id)?;
        if !req.view.state.is_open() {
            return None;
        }
        let answer = req.no_answer(why);
        Some(req.finish(why.into(), answer, now))
    }

    /// A waiting hook went away. The request ends as no longer needed when no other
    /// (joined) hook still waits for it.
    pub fn leave(&self, id: u64, waiter: u64, now: i64) -> Option<Done> {
        let mut inner = self.lock();
        let req = inner.requests.get_mut(&id)?;
        req.waiters
            .retain(|(w, tx)| *w != waiter && !tx.is_closed());
        if !req.view.state.is_open() || !req.waiters.is_empty() {
            return None;
        }
        let answer = req.no_answer(Ended::NoLongerNeeded);
        Some(req.finish(ApprovalState::NoLongerNeeded, answer, now))
    }

    /// The desk answered: ends this session's matching "Ask on my phone" requests. Never an
    /// "Ask me first" one, and never another session's.
    pub fn desk(&self, assistant: &str, session: &str, what: Desk, now: i64) -> Vec<Done> {
        let mut inner = self.lock();
        let mut out = Vec::new();
        for req in inner.requests.values_mut() {
            let ours = req.view.state.is_open()
                && req.view.why == AskWhy::AskOnPhone
                && req.view.assistant == assistant
                && req.session == session;
            let this_call = match what {
                Desk::ToolDone(hash) => req.input_hash == hash,
                Desk::TurnOver => true,
            };
            if ours && this_call {
                let answer = req.no_answer(Ended::AnsweredAtComputer);
                out.push(req.finish(ApprovalState::AnsweredAtComputer, answer, now));
            }
        }
        out
    }

    /// Open requests and those that ended in the last 10 minutes, newest first.
    pub fn list(&self, now: i64) -> Vec<ApprovalView> {
        let mut inner = self.lock();
        inner.forget_old(now);
        let mut out: Vec<ApprovalView> = inner.requests.values().map(|r| r.view.clone()).collect();
        out.sort_by(|a, b| b.opened_ms.cmp(&a.opened_ms).then(b.id.cmp(&a.id)));
        out
    }

    /// Whether an "Ask on my phone" request of this session is open (its push went out).
    pub fn waiting_in(&self, assistant: &str, session: &str) -> bool {
        self.lock().requests.values().any(|r| {
            r.view.state.is_open()
                && r.view.why == AskWhy::AskOnPhone
                && r.view.assistant == assistant
                && r.session == session
        })
    }

    pub fn get(&self, id: u64) -> Option<ApprovalView> {
        self.lock().requests.get(&id).map(|r| r.view.clone())
    }
}

impl Inner {
    fn forget(&mut self, assistant: &str, session: &str, hash: &Hash) {
        let key = (assistant.to_string(), session.to_string());
        if let Some(list) = self.remembered.get_mut(&key) {
            list.retain(|(h, _)| h != hash);
            if list.is_empty() {
                self.remembered.remove(&key);
            }
        }
    }

    fn forget_old(&mut self, now: i64) {
        self.requests
            .retain(|_, r| r.ended_ms.is_none_or(|e| now - e < ENDED_KEEP_MS));
        self.remembered.retain(|_, list| {
            list.retain(|(_, at)| now - at < REMEMBER_ALLOW_MS);
            !list.is_empty()
        });
    }

    /// Random, never 0, never one in use; never the same across agent runs.
    fn new_id(&self) -> u64 {
        loop {
            let mut b = [0u8; 8];
            if getrandom::fill(&mut b).is_err() {
                // No random numbers: the clock's nanoseconds still differ between runs.
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(1);
                b = t.to_le_bytes();
            }
            let id = u64::from_le_bytes(b);
            if id != 0 && !self.requests.contains_key(&id) {
                return id;
            }
        }
    }
}

#[cfg(test)]
#[path = "approvals_tests.rs"]
mod tests;
