use std::cell::RefCell;

use pocketgpu_proto::Permission;
use pocketgpu_proto::assistants::ToolCall;
use pocketgpu_proto::msg::jobs::{JobDetail, JobState, JobSummary};
use serde_json::{Value, json};

use super::*;

/// Answers like a running agent and records what it was asked.
#[derive(Default)]
struct Fake {
    asked: RefCell<Vec<Request>>,
    held: bool,
    log: Option<std::path::PathBuf>,
    /// The assistant's level, checked as the agent does.
    level: Option<Permission>,
}

impl Agent for Fake {
    async fn call(&self, req: Request) -> Result<Response> {
        self.asked.borrow_mut().push(req.clone());
        Ok(match req {
            Request::ListJobs { .. } => Response::Jobs(vec![summary(3, JobState::Running)]),
            Request::GetJob { id } => Response::Job {
                detail: Box::new(JobDetail {
                    summary: summary(id, JobState::Crashed),
                    command: "python train.py".into(),
                    peak_vram_mb: None,
                    log_bytes: 0,
                    alerts_on: true,
                    tail: vec![],
                }),
                log_path: self
                    .log
                    .as_ref()
                    .map_or("/nonexistent".into(), |p| p.display().to_string()),
            },
            Request::Notify { .. } => Response::Notified {
                phones: 2,
                channels: 1,
                held: self.held,
            },
            Request::AssistantTool {
                assistant, call, ..
            } => {
                let level = self.level.unwrap_or(Permission::View);
                match crate::assistants::tools::refusal(&assistant, level, &call) {
                    Some(why) => Response::Error(why),
                    None => match call {
                        ToolCall::OfferFile { .. } => Response::Offered {
                            name: "report.pdf".into(),
                            size: 3,
                            to: vec!["Pixel".into()],
                            expires_ms: 0,
                            pushed: vec![],
                        },
                        ToolCall::StopJob { .. } => Response::Ok,
                        ToolCall::RunAction { .. } => Response::ActionRan {
                            exit_code: Some(0),
                            output: "restarted".into(),
                            duration_ms: 1500,
                            timed_out: false,
                        },
                    },
                }
            }
            _ => Response::Error("the agent is not running".into()),
        })
    }
}

fn summary(id: u64, state: JobState) -> JobSummary {
    JobSummary {
        id,
        name: "train.py".into(),
        state,
        started_ms: 0,
        ended_ms: None,
        exit_code: None,
    }
}

async fn ask(server: &mut Server<Fake>, id: u64, method: &str, params: Value) -> Value {
    let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    server.handle(&msg.to_string()).await.expect("a reply")
}

async fn call(server: &mut Server<Fake>, name: &str, args: Value) -> (String, bool) {
    let r = ask(
        server,
        9,
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )
    .await;
    let text = r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    (text, r["result"]["isError"] == json!(true))
}

#[tokio::test]
async fn introduces_itself_and_its_tools() {
    let mut s = Server::new(Fake::default());
    let r = ask(
        &mut s,
        1,
        "initialize",
        json!({ "protocolVersion": "2025-03-26" }),
    )
    .await;
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(r["result"]["serverInfo"]["name"], "pocketgpu");
    // Long jobs go to the background, out of reach of the assistant's own timeout.
    let instructions = r["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("`pgpu run -- <command>` in the background"));
    assert!(instructions.contains("Never put secrets"));
    // An unknown version gets the newest one.
    let r = ask(
        &mut s,
        2,
        "initialize",
        json!({ "protocolVersion": "1999-01-01" }),
    )
    .await;
    assert_eq!(r["result"]["protocolVersion"], VERSIONS[0]);

    let r = ask(&mut s, 3, "tools/list", json!({})).await;
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "machine_status",
            "list_jobs",
            "job_output",
            "notify_phone",
            "offer_file",
            "stop_job",
            "run_action"
        ]
    );
}

#[tokio::test]
async fn notifications_get_no_reply_and_bad_input_an_error() {
    let mut s = Server::new(Fake::default());
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(s.handle(&note.to_string()).await.is_none());
    assert_eq!(
        s.handle("{not json").await.unwrap()["error"]["code"],
        -32700
    );
    let r = ask(&mut s, 4, "resources/list", json!({})).await;
    assert_eq!(r["error"]["code"], -32601);
}

#[tokio::test]
async fn tools_reach_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("job.log");
    std::fs::write(&log, "epoch 1\nepoch 2\nTraceback: out of memory\n").unwrap();
    let mut s = Server::new(Fake {
        log: Some(log),
        ..Fake::default()
    });

    let (text, err) = call(&mut s, "list_jobs", json!({ "limit": 500 })).await;
    assert!(!err && text.contains("train.py"));
    let (text, err) = call(&mut s, "job_output", json!({ "job": 3, "lines": 2 })).await;
    assert!(!err);
    assert!(text.starts_with("Job 3 is crashed"));
    assert!(text.ends_with("epoch 2\nTraceback: out of memory"));
    assert!(!text.contains("epoch 1"));

    let asked = s.agent.asked.borrow();
    // The limit is capped.
    assert_eq!(asked[0], Request::ListJobs { limit: 50 });
}

#[tokio::test]
async fn tool_problems_are_tool_errors() {
    let mut s = Server::new(Fake::default());
    let (text, err) = call(&mut s, "machine_status", json!({})).await;
    assert!(err && text.contains("not running"));
    let (_, err) = call(&mut s, "job_output", json!({})).await;
    assert!(err);
    let (_, err) = call(&mut s, "rm_rf", json!({})).await;
    assert!(err);
}

#[tokio::test]
async fn notes_reach_the_phone_at_most_ten_an_hour() {
    let mut s = Server::new(Fake::default());
    let (_, err) = call(&mut s, "notify_phone", json!({ "title": "Done" })).await;
    assert!(err, "a note needs a message");
    for _ in 0..NOTES_PER_HOUR {
        let (text, err) = call(
            &mut s,
            "notify_phone",
            json!({ "message": "Fine-tune finished" }),
        )
        .await;
        assert!(!err && text.contains("2 phone(s)"));
    }
    let (text, err) = call(&mut s, "notify_phone", json!({ "message": "again" })).await;
    assert!(err && text.contains("an hour"));
}

#[tokio::test]
async fn undelivered_notes_say_why() {
    struct Nobody;
    impl Agent for Nobody {
        async fn call(&self, _: Request) -> Result<Response> {
            Ok(Response::Notified {
                phones: 0,
                channels: 0,
                held: false,
            })
        }
    }
    let mut s = Server::new(Nobody);
    let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "notify_phone", "arguments": { "message": "done" } } });
    let r = s.handle(&msg.to_string()).await.unwrap();
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("pgpu pair")
    );
}

#[tokio::test]
async fn held_notes_say_so() {
    let mut s = Server::new(Fake {
        held: true,
        ..Fake::default()
    });
    let (text, err) = call(&mut s, "notify_phone", json!({ "message": "done" })).await;
    assert!(!err && text.contains("paused"));
    assert!(text.contains("AI assistant alerts off"), "{text}");
}

fn as_assistant(level: Option<Permission>, id: Option<&str>) -> Server<Fake> {
    Server::new(Fake {
        level,
        ..Fake::default()
    })
    .for_assistant(id.map(str::to_string), "/w/proj".into())
}

#[tokio::test]
async fn view_assistants_are_refused_with_how_to_change_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.pdf");
    std::fs::write(&file, "pdf").unwrap();
    let path = file.to_string_lossy().into_owned();
    let mut s = as_assistant(None, Some("claude"));
    for (name, args, need) in [
        ("offer_file", json!({ "path": path }), "control"),
        ("stop_job", json!({ "number": 3 }), "control"),
        ("run_action", json!({ "name": "restart vllm" }), "shell"),
    ] {
        let (text, err) = call(&mut s, name, args).await;
        assert!(err, "{name}: {text}");
        assert!(text.contains("Claude Code has View"), "{text}");
        assert!(
            text.contains(&format!("pgpu assistants set claude {need}")),
            "{text}"
        );
    }
    // Every call went to the agent with the id, the folder and the session's start.
    let asked = s.agent.asked.borrow();
    assert_eq!(asked.len(), 3);
    let Request::AssistantTool {
        assistant,
        folder,
        call,
        ..
    } = &asked[0]
    else {
        panic!("{:?}", asked[0]);
    };
    assert_eq!((assistant.as_str(), folder.as_str()), ("claude", "/w/proj"));
    assert!(
        matches!(call, ToolCall::OfferFile { path: p } if std::path::Path::new(p).is_absolute())
    );
}

#[tokio::test]
async fn raised_levels_let_the_tools_work() {
    let mut s = as_assistant(Some(Permission::Control), Some("codex"));
    let (text, err) = call(&mut s, "stop_job", json!({ "number": 3 })).await;
    assert!(!err && text.contains("job 3"), "{text}");
    let (text, err) = call(&mut s, "run_action", json!({ "name": "x" })).await;
    assert!(
        err && text.contains("pgpu assistants set codex shell"),
        "{text}"
    );

    let mut s = as_assistant(Some(Permission::Shell), Some("codex"));
    let (text, err) = call(&mut s, "run_action", json!({ "name": "restart" })).await;
    assert!(
        !err && text.contains("restart exited with 0 after 1.5 s"),
        "{text}"
    );
    assert!(text.ends_with("restarted"));
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.pdf");
    std::fs::write(&file, "pdf").unwrap();
    let args = json!({ "path": file.to_string_lossy() });
    let (text, err) = call(&mut s, "offer_file", args).await;
    assert!(!err && text.contains("offered to Pixel"), "{text}");
}

#[tokio::test]
async fn without_an_assistant_id_nothing_beyond_view_runs() {
    let mut s = as_assistant(Some(Permission::Shell), None);
    let (text, err) = call(&mut s, "stop_job", json!({ "number": 1 })).await;
    assert!(err && text.contains("--assistant"), "{text}");
    // View tools still work.
    let (_, err) = call(&mut s, "list_jobs", json!({})).await;
    assert!(!err);
}

#[tokio::test]
async fn bad_tool_arguments_never_reach_the_agent() {
    let mut s = as_assistant(Some(Permission::Shell), Some("claude"));
    for (name, args) in [
        ("offer_file", json!({})),
        ("offer_file", json!({ "path": "/no/such/file/here" })),
        ("stop_job", json!({ "number": "three" })),
        ("run_action", json!({ "name": " " })),
    ] {
        let (_, err) = call(&mut s, name, args).await;
        assert!(err, "{name}");
    }
    assert!(s.agent.asked.borrow().is_empty());
}
