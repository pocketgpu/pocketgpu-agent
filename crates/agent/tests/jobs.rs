//! Jobs end to end: `pgpu run` ↔ agent over the local socket, and phones over `pgpu/jobs/1`.

#![cfg(unix)]

mod harness;

use std::time::Duration;

use harness::Harness;
use pocketgpu::cli::Silent;
use pocketgpu::jobs::runner::{self, RunOptions};
use pocketgpu::jobs::store;
use pocketgpu_client::LogEvent;
use pocketgpu_proto::msg::jobs::{JobState, Request, Response};
use pocketgpu_proto::{ErrorCode, Permission};

/// An agent with its local socket open, so `pgpu run` can register jobs.
async fn agent_with_socket() -> Harness {
    let h = Harness::start().await;
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    for _ in 0..50 {
        if h.agent.paths.socket().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    h
}

fn opts(cmd: &[&str]) -> RunOptions {
    RunOptions {
        name: None,
        no_alert: false,
        command: cmd.iter().map(|s| s.to_string()).collect(),
    }
}

/// Exit code of `pgpu run`, as the process would report it.
fn code(result: anyhow::Result<()>) -> u8 {
    match result {
        Ok(()) => 0,
        Err(e) => e.downcast_ref::<Silent>().expect("a plain exit code").0,
    }
}

#[tokio::test]
async fn run_passes_the_exit_code_and_records_the_job() {
    let h = agent_with_socket().await;
    let mut ended = h.agent.jobs.subscribe();
    let r = runner::run(
        &h.agent.paths,
        false,
        opts(&["sh", "-c", "echo hello; exit 3"]),
    )
    .await;
    assert_eq!(code(r), 3);
    let jobs = store::list(&h.agent.db(), 10, None).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, JobState::Crashed);
    assert_eq!(jobs[0].exit_code, Some(3));
    assert_eq!(jobs[0].name, "sh");
    let (detail, _) = store::get(&h.agent.db(), jobs[0].id).unwrap().unwrap();
    assert_eq!(detail.tail, vec!["hello".to_string()]);
    let event = tokio::time::timeout(Duration::from_secs(2), ended.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.state, JobState::Crashed);

    assert_eq!(
        code(runner::run(&h.agent.paths, false, opts(&["true"])).await),
        0
    );
    assert_eq!(
        code(runner::run(&h.agent.paths, false, opts(&["false"])).await),
        1
    );
    let jobs = store::list(&h.agent.db(), 10, None).unwrap();
    assert_eq!(jobs[1].state, JobState::Finished);
}

#[tokio::test]
async fn run_works_without_an_agent() {
    let dir = tempfile::tempdir().unwrap();
    let paths = pocketgpu::store::paths::Paths::under(dir.path().to_path_buf());
    let r = runner::run(&paths, false, opts(&["sh", "-c", "exit 4"])).await;
    assert_eq!(code(r), 4);
}

#[tokio::test]
async fn stop_from_a_phone_ends_the_job() {
    let h = agent_with_socket().await;
    let paths = h.agent.paths.clone();
    let job = tokio::spawn(async move { runner::run(&paths, false, opts(&["sleep", "30"])).await });
    let id = loop {
        if let Some(j) = store::list(&h.agent.db(), 1, None).unwrap().first()
            && h.agent.jobs.is_running(j.id)
        {
            break j.id;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // Let the wrapper report its process group before stopping.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let viewer = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&viewer).await.unwrap();
    match machine.jobs(Request::StopJob { id }).await {
        Err(pocketgpu_client::ClientError::Remote(e)) => assert_eq!(e.code, ErrorCode::Forbidden),
        other => panic!("view must not stop jobs: {other:?}"),
    }

    let controller = h.phone(Some(Permission::Control)).await;
    let machine = h.connect(&controller).await.unwrap();
    assert_eq!(
        machine.jobs(Request::StopJob { id }).await.unwrap(),
        Response::Ok
    );
    let result = tokio::time::timeout(Duration::from_secs(5), job)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code(result), 128 + 15);
    let (detail, _) = store::get(&h.agent.db(), id).unwrap().unwrap();
    assert_eq!(detail.summary.state, JobState::Stopped);
    assert_eq!(h.audit_events("job_stop"), 1);
    assert_eq!(h.audit_events("refused"), 1);
}

#[tokio::test]
async fn phones_list_jobs_and_follow_logs() {
    let h = agent_with_socket().await;
    let r = runner::run(
        &h.agent.paths,
        false,
        opts(&["sh", "-c", "for i in 1 2 3; do echo line $i; done"]),
    )
    .await;
    assert_eq!(code(r), 0);
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();

    let Response::Jobs(jobs) = machine
        .jobs(Request::ListJobs {
            limit: 10,
            before: None,
        })
        .await
        .unwrap()
    else {
        panic!("expected Jobs");
    };
    let id = jobs[0].id;
    let Response::Job(detail) = machine.jobs(Request::GetJob { id }).await.unwrap() else {
        panic!("expected Job");
    };
    assert_eq!(detail.tail.last().map(String::as_str), Some("line 3"));
    assert!(matches!(
        machine.jobs(Request::GetJob { id: 999 }).await,
        Err(pocketgpu_client::ClientError::Remote(e)) if e.code == ErrorCode::NotFound
    ));

    let mut stream = machine.follow_log(id, 0).await.unwrap();
    let mut log = Vec::new();
    let end = loop {
        match stream.next().await.unwrap() {
            Some(LogEvent::Chunk { bytes, .. }) => log.extend(bytes),
            Some(LogEvent::End { exit_code }) => break exit_code,
            None => panic!("the stream closed before End"),
        }
    };
    assert_eq!(String::from_utf8_lossy(&log), "line 1\nline 2\nline 3\n");
    assert_eq!(end, Some(0));

    let refused = machine
        .jobs(Request::SetJobAlerts { id, on: false })
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("control"),
        "view may not change alerts"
    );
}

/// Needs a terminal: `script -qec "cargo test -p pocketgpu --test jobs -- --ignored" /dev/null`
#[tokio::test]
#[ignore = "needs a terminal"]
async fn run_in_a_terminal_gives_the_job_a_terminal() {
    let h = agent_with_socket().await;
    let r = runner::run(
        &h.agent.paths,
        false,
        opts(&["sh", "-c", "test -t 1 && printf 'TTY\\r50%%\\r100%%\\n'"]),
    )
    .await;
    assert_eq!(code(r), 0);
    let id = store::list(&h.agent.db(), 1, None).unwrap()[0].id;
    let (detail, log) = store::get(&h.agent.db(), id).unwrap().unwrap();
    let text = std::fs::read_to_string(log).unwrap();
    // The terminal may echo stray input first, as a real one would.
    assert!(text.contains("TTY"), "log was {text:?}");
    // Progress redraws collapse to what's on screen.
    assert_eq!(detail.tail, vec!["100%".to_string()]);
}
