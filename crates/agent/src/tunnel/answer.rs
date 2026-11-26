//! Chat answers the agent writes down as they come (F18, `tunnel::ToAgent::Answer`), so an
//! answer finishes on the machine when the phone switches apps or loses its connection, and
//! the phone picks it up again from where it was. Each is the phone's own (by its key and the
//! id it chose), kept in memory only, and forgotten 10 minutes after it ended.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::SendStream;
use pocketgpu_proto::share::ModelServerRef;
use pocketgpu_proto::tunnel::{
    AnswerAction, Method, ToPhone, permission_for, server_permission_for,
};
use pocketgpu_proto::{Error, ErrorCode, Permission, write_frame};
use tokio::sync::watch;

use super::{CHUNK, Call, base_for, refused, request};
use crate::daemon::auth::Peer;
use crate::daemon::state::Agent;

/// How long an ended answer is kept for the phone to pick up.
const KEEP: Duration = Duration::from_secs(10 * 60);
/// Answers kept per phone; the oldest go first.
const PER_PHONE: usize = 8;
/// An answer longer than this stops there.
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// The answers, by phone key and the phone's id.
#[derive(Default)]
pub struct Answers(Mutex<HashMap<(String, u64), Arc<Answer>>>);

pub struct Answer {
    state: Mutex<State>,
    /// Bumped on every change, to wake readers.
    changed: watch::Sender<u64>,
    task: Mutex<Option<tokio::task::AbortHandle>>,
    started: Instant,
}

#[derive(Default)]
struct State {
    /// The server's HTTP status, once it answers: loading a model can take a minute.
    status: Option<u16>,
    bytes: Vec<u8>,
    /// `Some` once the reply ended: `Ok`, or why it stopped.
    end: Option<Result<(), Error>>,
    ended: Option<Instant>,
}

impl Answer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            changed: watch::channel(0).0,
            task: Mutex::default(),
            started: Instant::now(),
        })
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().expect("answer lock poisoned"));
        self.changed.send_modify(|n| *n += 1);
    }

    fn push(&self, bytes: &[u8]) -> bool {
        let mut full = false;
        self.update(|s| {
            if s.bytes.len() + bytes.len() > MAX_BYTES {
                full = true;
                s.end = Some(Err(Error::new(
                    ErrorCode::Internal,
                    "the answer is too long",
                )));
                s.ended = Some(Instant::now());
            } else {
                s.bytes.extend_from_slice(bytes);
            }
        });
        !full
    }

    fn finish(&self, end: Result<(), Error>) {
        self.update(|s| {
            if s.end.is_none() {
                s.end = Some(end);
                s.ended = Some(Instant::now());
            }
        });
    }

    /// Stops writing it down; whoever is reading it hears so.
    fn stop(&self) {
        if let Some(task) = self.task.lock().expect("answer lock poisoned").take() {
            task.abort();
        }
        self.finish(Err(Error::new(
            ErrorCode::Unknown,
            "the answer was stopped",
        )));
    }

    fn gone(&self) -> bool {
        let s = self.state.lock().expect("answer lock poisoned");
        s.ended.is_some_and(|t| t.elapsed() > KEEP)
    }
}

impl Answers {
    fn get(&self, key: &(String, u64)) -> Option<Arc<Answer>> {
        let all = self.0.lock().expect("answers lock poisoned");
        all.get(key).filter(|a| !a.gone()).cloned()
    }

    /// Keeps `answer`, in place of one with the same id, and forgets old ones.
    fn insert(&self, key: (String, u64), answer: Arc<Answer>) {
        let mut all = self.0.lock().expect("answers lock poisoned");
        all.retain(|_, a| !a.gone());
        if let Some(old) = all.insert(key.clone(), answer) {
            old.stop();
        }
        let mut mine: Vec<_> = all
            .iter()
            .filter(|(k, _)| k.0 == key.0)
            .map(|(k, a)| (a.started, k.clone()))
            .collect();
        if mine.len() > PER_PHONE {
            mine.sort();
            for (_, k) in &mine[..mine.len() - PER_PHONE] {
                if let Some(a) = all.remove(k) {
                    a.stop();
                }
            }
        }
    }

    fn remove(&self, key: &(String, u64)) {
        if let Some(a) = self.0.lock().expect("answers lock poisoned").remove(key) {
            a.stop();
        }
    }
}

/// Starts, resumes or stops one of the phone's answers, streaming it back as for `Request`.
pub(super) async fn serve(
    agent: &Arc<Agent>,
    peer: &Peer,
    http: &reqwest::Client,
    action: AnswerAction,
    send: &mut SendStream,
) -> Result<(), Error> {
    let phone = peer.key.to_string();
    match action {
        AnswerAction::Start { id, port, body } => {
            let (server, path) = match port {
                None => (ModelServerRef::Ollama, "/api/chat"),
                Some(p) => (ModelServerRef::Port(p), "/v1/chat/completions"),
            };
            let needed = match port {
                None => permission_for(Method::Post, path),
                Some(_) => server_permission_for(Method::Post, path),
            }
            .ok_or_else(|| refused(Method::Post, path))?;
            agent.require(peer, needed, "chat")?;
            let (base, name) = base_for(agent, server)?;
            let call = Call {
                method: Method::Post,
                path,
                body,
            };
            // Kept before the server answers, as a model may take a minute to load: a phone
            // that loses its connection meanwhile picks the answer up too.
            let answer = Answer::new();
            let ask = write_down(answer.clone(), http.clone(), base, call, name);
            let task = tokio::spawn(ask);
            *answer.task.lock().expect("answer lock poisoned") = Some(task.abort_handle());
            agent.answers.insert((phone, id), answer.clone());
            read_out(&answer, 0, send).await
        }
        AnswerAction::Resume { id, from } => {
            agent.require(peer, Permission::Control, "chat")?;
            let answer = agent.answers.get(&(phone, id)).ok_or_else(|| {
                Error::new(
                    ErrorCode::NotFound,
                    "the computer no longer has that answer",
                )
            })?;
            read_out(&answer, from, send).await
        }
        AnswerAction::Stop { id } => {
            agent.answers.remove(&(phone, id));
            let gone = |_| Error::new(ErrorCode::Unknown, "the phone closed the request");
            write_frame(send, &ToPhone::Head { status: 200 })
                .await
                .map_err(gone)?;
            write_frame(send, &ToPhone::End).await.map_err(gone)
        }
    }
}

/// Asks the server and reads its reply to the end, whoever is listening.
async fn write_down(
    answer: Arc<Answer>,
    http: reqwest::Client,
    base: String,
    call: Call<'static>,
    name: String,
) {
    let mut res = match request(&http, &base, call, &name).await {
        Ok(res) => res,
        Err(e) => return answer.finish(Err(e)),
    };
    let status = res.status().as_u16();
    answer.update(|s| s.status = Some(status));
    loop {
        match res.chunk().await {
            Ok(Some(bytes)) => {
                if !answer.push(&bytes) {
                    return;
                }
            }
            Ok(None) => return answer.finish(Ok(())),
            Err(e) => {
                let why = format!("{name} stopped answering: {e}");
                return answer.finish(Err(Error::new(ErrorCode::Internal, why)));
            }
        }
    }
}

/// Sends the answer from byte `from`, once the server has answered: what is written down, then
/// the rest as it comes.
async fn read_out(answer: &Answer, from: u64, send: &mut SendStream) -> Result<(), Error> {
    let gone = |_| Error::new(ErrorCode::Unknown, "the phone closed the request");
    let mut at = usize::try_from(from).unwrap_or(usize::MAX);
    let mut changed = answer.changed.subscribe();
    let mut head = false;
    loop {
        // Seen before reading, so a change after the read wakes the wait below.
        changed.borrow_and_update();
        let (status, part, end) = {
            let s = answer.state.lock().expect("answer lock poisoned");
            let part = match s.status {
                Some(_) => s.bytes.get(at..).map(<[u8]>::to_vec).unwrap_or_default(),
                None => Vec::new(),
            };
            (s.status, part, s.end.clone())
        };
        if !head && let Some(status) = status {
            write_frame(send, &ToPhone::Head { status })
                .await
                .map_err(gone)?;
            head = true;
        }
        at += part.len();
        for piece in part.chunks(CHUNK) {
            write_frame(send, &ToPhone::Chunk(piece.to_vec()))
                .await
                .map_err(gone)?;
        }
        match end {
            Some(Ok(())) if head => return write_frame(send, &ToPhone::End).await.map_err(gone),
            Some(Ok(())) => return Err(Error::new(ErrorCode::Internal, "the answer stopped")),
            Some(Err(e)) => return Err(e),
            None => {
                if changed.changed().await.is_err() {
                    return Err(Error::new(ErrorCode::Internal, "the answer stopped"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_few_answers_per_phone() {
        let answers = Answers::default();
        for id in 0..10 {
            answers.insert(("a".into(), id), Answer::new());
        }
        answers.insert(("b".into(), 0), Answer::new());
        assert!(answers.get(&("a".into(), 0)).is_none());
        assert!(answers.get(&("a".into(), 1)).is_none());
        assert!(answers.get(&("a".into(), 9)).is_some());
        assert!(answers.get(&("b".into(), 0)).is_some());
        // Another phone's id is not this one's.
        assert!(answers.get(&("b".into(), 9)).is_none());
        answers.remove(&("a".into(), 9));
        assert!(answers.get(&("a".into(), 9)).is_none());
    }

    #[test]
    fn stops_a_runaway_answer() {
        let a = Answer::new();
        assert!(a.push(b"hello"));
        assert!(!a.push(&vec![0; MAX_BYTES]));
        let s = a.state.lock().unwrap();
        assert_eq!(s.bytes, b"hello");
        assert!(matches!(s.end, Some(Err(_))));
    }
}
