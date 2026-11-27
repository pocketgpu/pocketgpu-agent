//! PocketGPU's MCP tools that need more than View (F32, US4): `pgpu mcp --assistant <id>`
//! sends each one here with the assistant's id, and the agent checks the level stored for it
//! (missing means View) on every call, from the config as it is now. A refusal is audited as
//! `assistant_refused` and says which level is needed and how to change it.
//!
//! Assistants run as the same user and can run `pgpu` in a shell, so levels shape what
//! PocketGPU's own tools do; they are not a wall against a hostile assistant.

use std::path::Path;
use std::sync::Arc;

use pocketgpu_proto::Permission;
use pocketgpu_proto::assistants::ToolCall;
use pocketgpu_proto::local::{Request, Response};

use crate::audit::{self, Event};
use crate::daemon::state::Agent;

/// The id an MCP server started without `--assistant` sends.
pub const NO_ID: &str = "";

/// `AssistantTool`: checks the level, then does the work as `pgpu send`, `pgpu stop` or
/// `pgpu actions run` would.
pub async fn local_request(
    agent: &Arc<Agent>,
    assistant: &str,
    folder: &str,
    since_ms: i64,
    call: ToolCall,
) -> Response {
    let id = super::seen::canonical(assistant.trim());
    let level = level_of(agent, &id);
    if let Some(why) = refusal(&id, level, &call) {
        let who = if id.is_empty() { "unknown" } else { &id };
        let target = format!("{who} · {}", call.tool());
        if let Err(e) = audit::record(
            &agent.db(),
            "local",
            Event::AssistantRefused,
            &target,
            call.needs().as_str(),
        ) {
            tracing::warn!("could not write audit entry: {e}");
        }
        return Response::Error(why);
    }
    match call {
        ToolCall::OfferFile { path } => {
            if !Path::new(&path).is_absolute() {
                return Response::Error(format!("{path} is not a full path"));
            }
            let req = Request::Offer { path, to: None };
            crate::files::local_request(agent, req).await
        }
        ToolCall::StopJob { number } => stop_job(agent, &id, folder, since_ms, number),
        ToolCall::RunAction { name } => crate::actions::local::run(agent, &name, &id).await,
    }
}

/// The level stored for this assistant now; an unknown id is View.
fn level_of(agent: &Agent, id: &str) -> Permission {
    if id.is_empty() {
        return Permission::View;
    }
    agent
        .config
        .read()
        .expect("config lock poisoned")
        .assistants
        .level(id)
}

/// Why this call is refused at this level, with how to change it; `None` lets it run.
pub fn refusal(id: &str, level: Permission, call: &ToolCall) -> Option<String> {
    let need = call.needs();
    if id.is_empty() {
        return Some(format!(
            "{} needs to know which assistant is asking: PocketGPU's MCP server was started \
             without --assistant. Ask the user to run `pgpu assistants disconnect` and then \
             `pgpu assistants connect`.",
            call.tool()
        ));
    }
    if level >= need {
        return None;
    }
    Some(format!(
        "{} needs the {} level; {} has {}. The user can change it at the computer with \
         `pgpu assistants set {id} {}`, or on a Full control phone under AI assistants.",
        call.tool(),
        words(need),
        super::display_name(id),
        words(level),
        need.as_str(),
    ))
}

fn words(p: Permission) -> &'static str {
    match p {
        Permission::View => "View",
        Permission::Control => "Control",
        Permission::Shell => "Shell",
    }
}

/// Stops a running job started in this session's folder (or below) since it began.
fn stop_job(agent: &Agent, assistant: &str, folder: &str, since_ms: i64, number: u64) -> Response {
    let origin = crate::jobs::store::origin(&agent.db(), number);
    let (cwd, started) = match origin {
        Ok(Some(o)) => o,
        Ok(None) => return Response::Error(format!("no job {number}")),
        Err(e) => return Response::Error(format!("{e:#}")),
    };
    if !started_here(&cwd, started, folder, since_ms) {
        return Response::Error(format!(
            "job {number} wasn't started in this session; stop_job stops only jobs started \
             with `pgpu run` from this folder since the session began. Ask the user to stop it."
        ));
    }
    match agent.jobs.stop(agent, number) {
        Ok(()) => {
            let target = format!("job {number} · {assistant}");
            if let Err(e) = audit::record(&agent.db(), "local", Event::JobStop, &target, "ok") {
                tracing::warn!("could not write audit entry: {e}");
            }
            Response::Ok
        }
        Err(e) => Response::Error(e.to_string()),
    }
}

/// The job's folder is the session's folder or inside it, and it started after the session.
pub fn started_here(cwd: &str, started_ms: i64, folder: &str, since_ms: i64) -> bool {
    let folder = Path::new(folder);
    !folder.as_os_str().is_empty()
        && folder.is_absolute()
        && Path::new(cwd).starts_with(folder)
        && started_ms >= since_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_decide_and_the_refusal_says_how_to_change_them() {
        let offer = ToolCall::OfferFile { path: "/a".into() };
        let stop = ToolCall::StopJob { number: 1 };
        let run = ToolCall::RunAction { name: "x".into() };
        let why = refusal("claude", Permission::View, &offer).unwrap();
        assert!(why.contains("offer_file needs the Control level; Claude Code has View"));
        assert!(why.contains("pgpu assistants set claude control"));
        assert!(refusal("claude", Permission::Control, &offer).is_none());
        assert!(refusal("claude", Permission::Control, &stop).is_none());
        let why = refusal("codex", Permission::Control, &run).unwrap();
        assert!(why.contains("pgpu assistants set codex shell"));
        assert!(refusal("codex", Permission::Shell, &run).is_none());
        // No id: refused whatever the level.
        assert!(
            refusal(NO_ID, Permission::Shell, &offer)
                .unwrap()
                .contains("--assistant")
        );
    }

    #[test]
    fn only_jobs_from_this_session_stop() {
        let here = if cfg!(windows) {
            "C:\\w\\proj"
        } else {
            "/w/proj"
        };
        let below = if cfg!(windows) {
            "C:\\w\\proj\\sub"
        } else {
            "/w/proj/sub"
        };
        let other = if cfg!(windows) {
            "C:\\w\\project2"
        } else {
            "/w/project2"
        };
        assert!(started_here(here, 10, here, 10));
        assert!(started_here(below, 20, here, 10));
        assert!(!started_here(other, 20, here, 10), "a sibling folder");
        assert!(!started_here(here, 9, here, 10), "before the session");
        assert!(!started_here(here, 20, "", 10));
        assert!(!started_here(here, 20, "proj", 10), "a relative folder");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_agent_checks_the_level_on_every_call_and_audits_refusals() {
        use pocketgpu_proto::msg::control::NewAction;

        use crate::collect::fake::FakeGpus;

        let agent = Agent::in_memory(Arc::new(FakeGpus::new(1))).unwrap();
        let new = NewAction {
            name: "hello".into(),
            command: "echo hello".into(),
            icon: "play".into(),
            risky: false,
            timeout_s: Some(10),
        };
        assert!(matches!(
            crate::actions::local::add(&agent, &new),
            Response::Action(_)
        ));
        let run = || ToolCall::RunAction {
            name: "hello".into(),
        };
        // Missing means View.
        let Response::Error(why) = local_request(&agent, "claude-code", "/w", 0, run()).await
        else {
            panic!("ran at View");
        };
        assert!(why.contains("pgpu assistants set claude shell"), "{why}");
        let rows = audit::since(&agent.db(), 0).unwrap();
        let refused = rows
            .iter()
            .find(|r| r.event == "assistant_refused")
            .unwrap();
        assert_eq!(refused.target, "claude · run_action");
        assert_eq!(refused.result, "shell");

        // Raised: read again at the next call.
        let set_level = |level: &str| {
            agent
                .config
                .write()
                .unwrap()
                .assistants
                .levels
                .insert("claude".into(), level.into());
        };
        set_level("control");
        assert!(matches!(
            local_request(&agent, "claude", "/w", 0, run()).await,
            Response::Error(_)
        ));
        let stop = ToolCall::StopJob { number: 99 };
        let Response::Error(why) = local_request(&agent, "claude", "/w", 0, stop).await else {
            panic!("stopped a missing job");
        };
        assert_eq!(why, "no job 99");
        set_level("shell");
        let Response::ActionRan {
            exit_code, output, ..
        } = local_request(&agent, "claude", "/w", 0, run()).await
        else {
            panic!("didn't run");
        };
        assert_eq!(exit_code, Some(0));
        assert!(output.contains("hello"));
        let rows = audit::since(&agent.db(), 0).unwrap();
        assert!(rows.iter().any(|r| r.event == "action_run"
            && r.target.ends_with("hello · claude")
            && r.device == "local"));
        // Back to View: refused again.
        set_level("view");
        let offer = ToolCall::OfferFile {
            path: "/etc/hosts".into(),
        };
        assert!(matches!(
            local_request(&agent, "claude", "/w", 0, offer).await,
            Response::Error(e) if e.contains("Control level")
        ));
    }
}
