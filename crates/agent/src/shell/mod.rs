//! Terminal sessions (F16): at most 3 per phone, kept for 10 minutes after the phone detaches.
//!
//! A session's output goes to the one stream attached to it and into a replay buffer, so a
//! phone that reattaches sees the recent screen. A reaper runs while any session exists: it
//! updates titles, closes sessions detached too long, and closes those whose phone lost
//! Shell permission or was revoked.

pub mod handler;
mod pty;
mod tools;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::PublicKey;
use pocketgpu_proto::shell::{DETACH_TIMEOUT_S, MAX_SESSIONS_PER_PHONE, SessionInfo};
use pocketgpu_proto::{Error, ErrorCode, Permission};
use tokio::sync::mpsc;

use crate::audit::{self, Event as Audit};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

/// Recent output kept for a reattach.
const REPLAY_BYTES: usize = 64 * 1024;
/// Output chunks queued for a slow phone before the shell is paused.
const QUEUE: usize = 256;

/// What an attached stream receives.
#[derive(Debug)]
pub enum Event {
    Output(Vec<u8>),
    Title(String),
    Exited(Option<i32>),
    /// The agent closed the session; the text says why.
    Closed(Error),
}

pub struct Shells {
    table: Mutex<Table>,
    next_id: AtomicU64,
    next_attach: AtomicU64,
    detach_timeout: Mutex<Duration>,
}

#[derive(Default)]
struct Table {
    sessions: HashMap<u64, Arc<Session>>,
    reaper: bool,
}

pub struct Session {
    pub id: u64,
    pub owner: PublicKey,
    started: Instant,
    started_ms: i64,
    pty: pty::Pty,
    state: Mutex<State>,
    closed: AtomicBool,
}

#[derive(Default)]
struct State {
    replay: VecDeque<u8>,
    attached: Option<(u64, mpsc::Sender<Event>)>,
    detached_at: Option<Instant>,
    title: String,
}

impl Default for Shells {
    fn default() -> Self {
        Self {
            table: Mutex::default(),
            next_id: AtomicU64::new(1),
            next_attach: AtomicU64::new(1),
            detach_timeout: Mutex::new(Duration::from_secs(DETACH_TIMEOUT_S)),
        }
    }
}

impl Shells {
    /// Starts a new session for `owner`, refusing a fourth one.
    pub fn open(
        &self,
        agent: &Arc<Agent>,
        owner: PublicKey,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Session>, Error> {
        let mut table = self.table.lock().expect("shell lock");
        // Checked again under the table lock: `pgpu shell off` saves the setting first and
        // then closes what is in the table, so a session can't slip in between.
        if !agent.config.read().expect("config lock poisoned").shell {
            return Err(Error::new(
                ErrorCode::Forbidden,
                "terminal access was turned off at the machine",
            ));
        }
        let mine = table.sessions.values().filter(|s| s.owner == owner).count();
        if mine >= MAX_SESSIONS_PER_PHONE {
            return Err(Error::new(
                ErrorCode::Busy,
                format!(
                    "this phone already has {MAX_SESSIONS_PER_PHONE} terminal sessions; close one first"
                ),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (pty, reader) = pty::Pty::spawn(cols, rows)
            .map_err(|e| Error::new(ErrorCode::Internal, format!("{e:#}")))?;
        let session = Arc::new(Session {
            id,
            owner,
            started: Instant::now(),
            started_ms: now_ms(),
            state: Mutex::new(State {
                title: pty.title(),
                ..State::default()
            }),
            pty,
            closed: AtomicBool::new(false),
        });
        table.sessions.insert(id, session.clone());
        let (agent_ref, me) = (Arc::downgrade(agent), Arc::downgrade(&session));
        let started = reader.start(move |out| {
            let (Some(agent), Some(session)) = (agent_ref.upgrade(), me.upgrade()) else {
                return;
            };
            match out {
                pty::Output::Bytes(bytes) => session.output(bytes),
                pty::Output::Exited(code) => {
                    session.send(Event::Exited(code));
                    let how = code.map_or("ended".to_string(), |c| format!("exited {c}"));
                    agent.shells.finish(&agent, id, &how);
                }
            }
        });
        if let Err(e) = started {
            table.sessions.remove(&id);
            session.pty.kill();
            return Err(Error::new(ErrorCode::Internal, format!("{e:#}")));
        }
        if !table.reaper {
            table.reaper = true;
            tokio::spawn(reap(agent.clone()));
        }
        drop(table);
        record(agent, &owner, Audit::ShellOpen, id, "ok");
        Ok(session)
    }

    /// `owner`'s session with this id.
    pub fn get(&self, owner: &PublicKey, id: u64) -> Option<Arc<Session>> {
        let table = self.table.lock().expect("shell lock");
        table
            .sessions
            .get(&id)
            .filter(|s| &s.owner == owner)
            .cloned()
    }

    pub fn list(&self, owner: &PublicKey) -> Vec<SessionInfo> {
        let table = self.table.lock().expect("shell lock");
        let mut out: Vec<_> = table
            .sessions
            .values()
            .filter(|s| &s.owner == owner)
            .map(|s| s.info())
            .collect();
        out.sort_by_key(|s| s.session);
        out
    }

    pub fn count(&self) -> usize {
        self.table.lock().expect("shell lock").sessions.len()
    }

    /// Kills a session and tells its phone why.
    pub fn close(&self, agent: &Arc<Agent>, id: u64, why: Option<Error>, how: &str) {
        let session = self
            .table
            .lock()
            .expect("shell lock")
            .sessions
            .get(&id)
            .cloned();
        if let Some(s) = session {
            if let Some(e) = why {
                s.send(Event::Closed(e));
            }
            s.pty.kill();
            self.finish(agent, id, how);
        }
    }

    /// Closes every session, for `pgpu shell off`.
    pub fn close_all(&self, agent: &Arc<Agent>, how: &str) {
        let ids: Vec<u64> = self
            .table
            .lock()
            .expect("shell lock")
            .sessions
            .keys()
            .copied()
            .collect();
        let why = Error::new(
            ErrorCode::Forbidden,
            "terminal access was turned off at the machine",
        );
        for id in ids {
            self.close(agent, id, Some(why.clone()), how);
        }
    }

    /// For tests: how long a detached session waits.
    pub fn set_detach_timeout(&self, d: Duration) {
        *self.detach_timeout.lock().expect("shell lock") = d;
    }

    /// Removes a session and audits its end, once.
    fn finish(&self, agent: &Arc<Agent>, id: u64, how: &str) {
        let removed = self.table.lock().expect("shell lock").sessions.remove(&id);
        if let Some(s) = removed
            && !s.closed.swap(true, Ordering::SeqCst)
        {
            let took = format_duration(s.started.elapsed());
            record(
                agent,
                &s.owner,
                Audit::ShellClose,
                id,
                &format!("{how} after {took}"),
            );
        }
    }
}

impl Session {
    /// Attaches a stream, replacing any other one, and returns the recent output.
    pub fn attach(&self, shells: &Shells) -> (u64, mpsc::Receiver<Event>, Vec<u8>) {
        let (tx, rx) = mpsc::channel(QUEUE);
        let id = shells.next_attach.fetch_add(1, Ordering::Relaxed);
        let mut st = self.state.lock().expect("shell lock");
        st.attached = Some((id, tx));
        st.detached_at = None;
        (id, rx, st.replay.iter().copied().collect())
    }

    /// Detaches the stream `attach`, unless another one took over since.
    pub fn detach(&self, attach: u64) {
        let mut st = self.state.lock().expect("shell lock");
        if st.attached.as_ref().is_some_and(|(id, _)| *id == attach) {
            st.attached = None;
            st.detached_at = Some(Instant::now());
        }
    }

    pub fn write(&self, bytes: Vec<u8>) {
        self.pty.write(bytes);
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.pty.resize(cols, rows);
    }

    pub fn title(&self) -> String {
        self.state.lock().expect("shell lock").title.clone()
    }

    fn info(&self) -> SessionInfo {
        let st = self.state.lock().expect("shell lock");
        SessionInfo {
            session: self.id,
            title: st.title.clone(),
            started_ms: self.started_ms,
            attached: st.attached.is_some(),
        }
    }

    /// Called on the output thread: keep it for a reattach and pass it on. Waiting on a full
    /// queue pauses the shell until the phone catches up.
    fn output(&self, bytes: Vec<u8>) {
        let tx = {
            let mut st = self.state.lock().expect("shell lock");
            st.replay.extend(&bytes);
            let extra = st.replay.len().saturating_sub(REPLAY_BYTES);
            st.replay.drain(..extra);
            st.attached.as_ref().map(|(_, tx)| tx.clone())
        };
        if let Some(tx) = tx {
            let _ = tx.blocking_send(Event::Output(bytes));
        }
    }

    fn send(&self, event: Event) {
        let tx = self
            .state
            .lock()
            .expect("shell lock")
            .attached
            .as_ref()
            .map(|(_, tx)| tx.clone());
        if let Some(tx) = tx {
            let _ = tx.try_send(event);
        }
    }

    /// Updates the title, telling the attached phone when it changes.
    fn refresh_title(&self) {
        let title = self.pty.title();
        let changed = {
            let mut st = self.state.lock().expect("shell lock");
            (st.title != title)
                .then(|| st.title = title.clone())
                .is_some()
        };
        if changed {
            self.send(Event::Title(title));
        }
    }

    fn detached_for(&self) -> Option<Duration> {
        self.state
            .lock()
            .expect("shell lock")
            .detached_at
            .map(|t| t.elapsed())
    }
}

/// Runs while any session exists.
async fn reap(agent: Arc<Agent>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut n: u64 = 0;
    loop {
        tick.tick().await;
        n += 1;
        let sessions: Vec<Arc<Session>> = {
            let mut table = agent.shells.table.lock().expect("shell lock");
            if table.sessions.is_empty() {
                table.reaper = false;
                return;
            }
            table.sessions.values().cloned().collect()
        };
        let timeout = *agent.shells.detach_timeout.lock().expect("shell lock");
        if !agent.config.read().expect("config lock poisoned").shell {
            agent
                .shells
                .close_all(&agent, "closed: terminal access turned off");
            continue;
        }
        for s in sessions {
            s.refresh_title();
            if s.detached_for().is_some_and(|d| d >= timeout) {
                agent
                    .shells
                    .close(&agent, s.id, None, "closed while detached");
            } else if n.is_multiple_of(3)
                && agent
                    .permission_of(&s.owner)
                    .is_none_or(|p| p < Permission::Shell)
            {
                let why = Error::new(
                    ErrorCode::Forbidden,
                    "this phone no longer has Shell permission",
                );
                agent
                    .shells
                    .close(&agent, s.id, Some(why), "closed: permission lost");
            }
        }
    }
}

fn record(agent: &Agent, owner: &PublicKey, event: Audit, id: u64, result: &str) {
    if let Err(e) = audit::record(
        &agent.db(),
        &owner.to_string(),
        event,
        &format!("session {id}"),
        result,
    ) {
        tracing::warn!("could not write audit entry: {e}");
    }
}

fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}
