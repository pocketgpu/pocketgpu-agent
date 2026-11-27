//! `AssistantCheck` and `AssistantAsk` on the local stream (F32, contracts §1): the first
//! reply at once (`AssistantAnswer(Pass)`, an answer, or `AssistantHeld`), then for a held
//! action the final `AssistantAnswer` when it ends. If the hook goes away first, the
//! request ends as no longer needed. Each connection has its own task, so a waiting hook
//! never holds up other local requests.

use std::sync::Arc;

use pocketgpu_proto::assistants::{Answer, AskWhy, Ended};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::{read_frame, write_frame};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::assistants::hold::{self, Call, First};
use crate::daemon::state::Agent;

/// The hook's call, for the two assistant requests only.
pub fn call(req: Request) -> Option<Call> {
    Some(match req {
        Request::AssistantCheck {
            assistant,
            session,
            permission_mode,
            tool,
            input,
            oversize,
            folder,
        } => Call {
            why: AskWhy::AskFirst,
            assistant,
            session,
            permission_mode,
            tool,
            input,
            oversize,
            folder,
        },
        Request::AssistantAsk {
            assistant,
            session,
            tool,
            input,
            folder,
        } => Call {
            why: AskWhy::AskOnPhone,
            assistant,
            session,
            permission_mode: String::new(),
            tool,
            input,
            oversize: false,
            folder,
        },
        _ => return None,
    })
}

/// Answers one call, waiting on this stream while it is held. The stream ends after.
pub async fn serve<R, W>(agent: &Arc<Agent>, call: Call, mut r: R, mut w: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let held = match hold::start(agent, call) {
        First::Answer(answer) => {
            let _ = write_frame(&mut w, &Response::AssistantAnswer(answer)).await;
            return;
        }
        First::Held(held) => held,
    };
    let (id, waiter) = (held.id, held.waiter);
    if write_frame(&mut w, &Response::AssistantHeld { id })
        .await
        .is_err()
    {
        hold::leave(agent, id, waiter);
        return;
    }
    // The hook sends nothing more; the read ends when its socket closes.
    let gone = async { while let Ok(Some(_)) = read_frame::<_, Request>(&mut r).await {} };
    tokio::select! {
        answer = held.rx => {
            // A dropped sender is never an allow.
            let answer = answer.unwrap_or(Answer::NoDecision { why: Ended::NoLongerNeeded });
            if write_frame(&mut w, &Response::AssistantAnswer(answer)).await.is_err() {
                hold::leave(agent, id, waiter);
            }
        }
        () = gone => hold::leave(agent, id, waiter),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::collect::fake::FakeGpus;
    use crate::store::config::AssistantConfig;
    use pocketgpu_proto::assistants::ApprovalState;

    fn check(cmd: &str) -> Request {
        Request::AssistantCheck {
            assistant: "claude".into(),
            session: "s1".into(),
            permission_mode: "default".into(),
            tool: "Bash".into(),
            input: serde_json::json!({ "command": cmd }).to_string(),
            oversize: false,
            folder: "/w".into(),
        }
    }

    async fn agent_asking_first() -> Arc<Agent> {
        let agent = Agent::in_memory(Arc::new(FakeGpus::new(1))).unwrap();
        let cfg = AssistantConfig {
            ask_first: vec!["delete".into()],
            ..Default::default()
        };
        hold::save_settings(&agent, cfg, "local").unwrap();
        agent
    }

    /// A stream pair: the agent serves one end, the test is the hook on the other.
    fn connect(agent: &Arc<Agent>) -> tokio::net::UnixStream {
        let (hook, served) = tokio::net::UnixStream::pair().unwrap();
        tokio::spawn(super::super::serve_stream(agent.clone(), served));
        hook
    }

    async fn reply(s: &mut tokio::net::UnixStream) -> Option<Response> {
        tokio::time::timeout(std::time::Duration::from_secs(5), read_frame(s))
            .await
            .expect("a reply in time")
            .unwrap()
    }

    #[tokio::test]
    async fn a_non_match_passes_at_once_and_the_stream_ends() {
        let agent = agent_asking_first().await;
        let mut s = connect(&agent);
        write_frame(&mut s, &check("ls -la")).await.unwrap();
        assert_eq!(
            reply(&mut s).await,
            Some(Response::AssistantAnswer(Answer::Pass))
        );
        assert_eq!(reply(&mut s).await, None);
    }

    #[tokio::test]
    async fn a_held_action_gets_two_replies_and_never_an_allow_without_a_phone() {
        let agent = agent_asking_first().await;
        let mut s = connect(&agent);
        write_frame(&mut s, &check("rm -rf build")).await.unwrap();
        let Some(Response::AssistantHeld { id }) = reply(&mut s).await else {
            panic!("not held");
        };
        // No push yet (T020): no phone reached, so the desk prompt in default mode.
        assert_eq!(
            reply(&mut s).await,
            Some(Response::AssistantAnswer(Answer::NoDecision {
                why: Ended::NoPhone
            }))
        );
        let view = agent.assistants.approvals.get(id).unwrap();
        assert_eq!(view.state, ApprovalState::NoPhone);
        assert_eq!(view.detail, "rm -rf build");
    }

    #[tokio::test]
    async fn a_closed_hook_ends_the_request_and_others_go_on() {
        let agent = agent_asking_first().await;
        // Held directly in the store, so no push task ends it first.
        let mut s = connect(&agent);
        let opened = agent.assistants.approvals.open(
            crate::assistants::approvals::NewRequest {
                why: AskWhy::AskFirst,
                kind: pocketgpu_proto::assistants::Kind::Delete,
                assistant: "claude".into(),
                session: "s9".into(),
                permission_mode: "default".into(),
                tool: "Bash".into(),
                detail_text: "rm x".into(),
                input_hash: crate::assistants::approvals::input_hash("Bash", "rm x"),
                folder: "/w".into(),
                summary: "rm x".into(),
            },
            crate::store::db::now_ms(),
        );
        let crate::assistants::approvals::Opened::Held(held) = opened else {
            panic!("not held")
        };
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let (r, w) = tokio::io::split(b);
        // Serve a hold that joins the one above, then drop the hook's end.
        let served = {
            let agent = agent.clone();
            let call = call(Request::AssistantCheck {
                assistant: "claude".into(),
                session: "s9".into(),
                permission_mode: "default".into(),
                tool: "Bash".into(),
                input: "rm x".into(),
                oversize: false,
                folder: "/w".into(),
            })
            .unwrap();
            tokio::spawn(async move { serve(&agent, call, r, w).await })
        };
        // Another local request is answered meanwhile.
        write_frame(&mut s, &Request::Status).await.unwrap();
        assert!(matches!(reply(&mut s).await, Some(Response::Status(_))));
        drop(a);
        served.await.unwrap();
        let id = held.id;
        // The first hook still waits, so the request is open; it ends when that one leaves.
        assert!(agent.assistants.approvals.get(id).unwrap().state.is_open());
        hold::leave(&agent, id, held.waiter);
        assert_eq!(
            agent.assistants.approvals.get(id).unwrap().state,
            ApprovalState::NoLongerNeeded
        );
    }
}
