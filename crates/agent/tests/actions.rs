//! Quick actions and process kill over a real connection (F09, F10, F11).
// The process-kill test needs Linux; the rest runs everywhere.
#![cfg_attr(not(target_os = "linux"), allow(unused_imports, dead_code))]

mod harness;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness::Harness;
use pocketgpu::collect::fake::FakeGpus;
use pocketgpu::collect::{GpuCollector, GpuProcess};
use pocketgpu_client::ClientError;
use pocketgpu_proto::local::{Request as LocalRequest, Response as LocalResponse};
use pocketgpu_proto::msg::control::{NewAction, Request, Response};
use pocketgpu_proto::{ErrorCode, GpuInfo, GpuSample, Permission};

fn new_action(name: &str, command: &str) -> NewAction {
    NewAction {
        name: name.into(),
        command: command.into(),
        icon: "zap".into(),
        risky: false,
        timeout_s: None,
    }
}

fn code(res: Result<Response, ClientError>) -> ErrorCode {
    match res {
        Err(ClientError::Remote(e)) => e.code,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn control_runs_actions_but_only_shell_creates_them() {
    let h = Harness::start().await;
    let shell = h
        .connect(&h.phone(Some(Permission::Shell)).await)
        .await
        .unwrap();
    let control = h
        .connect(&h.phone(Some(Permission::Control)).await)
        .await
        .unwrap();

    // Actions run in sh on Linux and PowerShell on Windows.
    let say_hi = if cfg!(windows) {
        r#"echo "hi from $env:PGPU_ACTION"; exit 2"#
    } else {
        "echo hi from $PGPU_ACTION; exit 2"
    };
    let req = Request::CreateAction(new_action("Say hi", say_hi));
    // A saved command is terminal access too: refused until it is on at the machine.
    assert_eq!(code(shell.control(req.clone()).await), ErrorCode::Forbidden);
    let on = pocketgpu::ipc::handle(&h.agent, LocalRequest::SetShell { on: true });
    assert!(
        matches!(on, LocalResponse::Shell { enabled: true, .. }),
        "{on:?}"
    );
    assert_eq!(
        code(control.control(req.clone()).await),
        ErrorCode::Forbidden
    );
    let Response::Action(action) = shell.control(req.clone()).await.unwrap() else {
        panic!("expected the new action");
    };
    // Names are unique per machine.
    assert_eq!(code(shell.control(req).await), ErrorCode::Invalid);

    let Response::Actions(listed) = control.control(Request::ListActions).await.unwrap() else {
        panic!("expected actions");
    };
    assert_eq!(listed, vec![action.clone()]);

    let res = control
        .control(Request::RunAction { id: action.id })
        .await
        .unwrap();
    let Response::ActionResult {
        exit_code, output, ..
    } = res
    else {
        panic!("expected a result, got {res:?}");
    };
    assert_eq!(exit_code, Some(2));
    assert!(output.contains("hi from Say hi"), "{output}");

    let Response::Runs(runs) = control
        .control(Request::ListRuns { limit: 5 })
        .await
        .unwrap()
    else {
        panic!("expected runs");
    };
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].exit_code, Some(2));
    assert_eq!(runs[0].device_name.as_deref(), Some("Test phone"));

    let edit = Request::EditAction {
        id: action.id,
        action: new_action("Say hello", "echo hello"),
    };
    assert_eq!(
        code(control.control(edit.clone()).await),
        ErrorCode::Forbidden
    );
    shell.control(edit).await.unwrap();
    let delete = Request::DeleteAction { id: action.id };
    assert_eq!(
        code(control.control(delete.clone()).await),
        ErrorCode::Forbidden
    );
    shell.control(delete).await.unwrap();

    // One entry per state-changing request, plus one per refusal.
    assert_eq!(h.audit_events("action_create"), 2);
    assert_eq!(h.audit_events("action_run"), 1);
    assert_eq!(h.audit_events("action_edit"), 1);
    assert_eq!(h.audit_events("action_delete"), 1);
    assert_eq!(h.audit_events("refused"), 3);
    let entries = pocketgpu::audit::since(&h.agent.db(), 0).unwrap();
    let run = entries.iter().find(|e| e.event == "action_run").unwrap();
    assert_eq!(run.result, "exit 2");
    assert!(
        entries.iter().all(|e| !e.target.contains("echo")),
        "the audit log never holds command text"
    );
}

#[tokio::test]
async fn unknown_actions_are_not_found() {
    let h = Harness::start().await;
    let phone = h
        .connect(&h.phone(Some(Permission::Control)).await)
        .await
        .unwrap();
    let res = phone.control(Request::RunAction { id: 99 }).await;
    assert_eq!(code(res), ErrorCode::NotFound);
    assert_eq!(h.audit_events("action_run"), 1);
}

/// Two fake GPUs plus whatever processes the test puts on them.
struct WithProcesses {
    fake: FakeGpus,
    procs: Mutex<Vec<GpuProcess>>,
}

impl GpuCollector for WithProcesses {
    fn info(&self) -> Vec<GpuInfo> {
        self.fake.info()
    }
    fn sample(&self) -> Vec<GpuSample> {
        self.fake.sample()
    }
    fn processes(&self) -> Vec<GpuProcess> {
        self.procs.lock().unwrap().clone()
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn kill_stops_an_own_gpu_process() {
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let pid = child.id();
    let gpus = Arc::new(WithProcesses {
        fake: FakeGpus::new(2),
        procs: Mutex::new(vec![
            GpuProcess {
                pid,
                gpu: 1,
                vram_mb: Some(2_000),
                label: None,
            },
            GpuProcess {
                pid: std::process::id(),
                gpu: 0,
                vram_mb: Some(9_000),
                label: None,
            },
        ]),
    });
    let h = Harness::start_with(gpus).await;
    let view = h
        .connect(&h.phone(Some(Permission::View)).await)
        .await
        .unwrap();
    let control = h
        .connect(&h.phone(Some(Permission::Control)).await)
        .await
        .unwrap();

    assert_eq!(
        code(view.control(Request::ListProcesses).await),
        ErrorCode::Forbidden
    );
    let Response::Processes(procs) = control.control(Request::ListProcesses).await.unwrap() else {
        panic!("expected processes");
    };
    // Largest VRAM first; the agent's own process is never killable.
    assert_eq!(procs[0].pid, std::process::id());
    assert!(!procs[0].killable);
    assert_eq!(procs[1].pid, pid);
    assert!(procs[1].killable);
    assert!(
        procs[1].command.contains("sleep 60"),
        "{}",
        procs[1].command
    );

    assert_eq!(
        code(
            control
                .control(Request::KillProcess {
                    pid: std::process::id()
                })
                .await
        ),
        ErrorCode::Forbidden
    );
    // Not a GPU process: refused, even though it's ours.
    assert_eq!(
        code(
            control
                .control(Request::KillProcess { pid: pid + 100_000 })
                .await
        ),
        ErrorCode::NotFound
    );
    control.control(Request::KillProcess { pid }).await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(s) = child.try_wait().unwrap() {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("SIGTERM should stop sleep at once");
    assert!(!status.success());
    assert_eq!(h.audit_events("kill"), 3);
}

#[tokio::test]
async fn the_command_line_manages_actions_and_reads_the_audit_log() {
    use pocketgpu::ipc::handle;
    use pocketgpu_proto::local::{ActionEdit, Request as Local, Response as Reply};

    let h = Harness::start().await;
    let add = Local::AddAction(new_action("Disk usage", "df -h"));
    let Reply::Action(a) = handle(&h.agent, add) else {
        panic!("expected the new action");
    };
    let edit = Local::EditAction(ActionEdit {
        action: "disk usage".into(),
        risky: Some(true),
        ..Default::default()
    });
    let Reply::Action(b) = handle(&h.agent, edit) else {
        panic!("expected the edited action");
    };
    assert_eq!((b.id, b.risky, b.command.as_str()), (a.id, true, "df -h"));
    let bad = Local::EditAction(ActionEdit {
        action: a.id.to_string(),
        icon: Some("rocket".into()),
        ..Default::default()
    });
    assert!(matches!(handle(&h.agent, bad), Reply::Error(e) if e.contains("unknown icon")));
    let rm = Local::RemoveAction {
        action: "Disk usage".into(),
    };
    assert_eq!(handle(&h.agent, rm), Reply::Ok);
    assert!(matches!(handle(&h.agent, Local::ListActions), Reply::Actions(l) if l.is_empty()));

    let Reply::Audit(entries) = handle(&h.agent, Local::Audit { since_ms: 0 }) else {
        panic!("expected audit entries");
    };
    let events: Vec<_> = entries.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(events, ["action_delete", "action_edit", "action_create"]);
    assert!(entries.iter().all(|e| e.device == "local"));
}
