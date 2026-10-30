//! F16 terminal over `pgpu/shell/1`: a real agent, a real phone and a real shell. The commands
//! are POSIX shell; Windows (PowerShell) is checked by hand (specs/F21-windows).
#![cfg(unix)]

mod harness;

use std::time::Duration;

use harness::Harness;
use pocketgpu::ipc::handle;
use pocketgpu_client::{ClientError, Machine, ShellEvent, ShellReceiver, ShellSender};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::{ErrorCode, Permission};

fn set_shell(h: &Harness, on: bool) {
    let reply = handle(&h.agent, Request::SetShell { on });
    assert!(
        matches!(reply, Response::Shell { enabled, .. } if enabled == on),
        "{reply:?}"
    );
}

/// Output until `want` appears, or the text so far if the stream ends first.
async fn read_until(rx: &mut ShellReceiver, want: &str) -> String {
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains(want) {
        match tokio::time::timeout_at(deadline, rx.next()).await {
            Ok(Ok(Some(ShellEvent::Output(b)))) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(Ok(Some(_))) => {}
            other => panic!("waiting for {want:?}, got {other:?} after {seen:?}"),
        }
    }
    seen
}

async fn open(m: &Machine, session: Option<u64>) -> (u64, ShellSender, ShellReceiver) {
    let (opened, tx, rx) = m.shell(80, 24, session).await.expect("shell opens");
    (opened.session, tx, rx)
}

fn refused_code(r: Result<impl std::fmt::Debug, ClientError>) -> ErrorCode {
    match r {
        Err(ClientError::Remote(e)) => e.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_terminal_is_off_until_turned_on_and_needs_shell_permission() {
    let h = Harness::start().await;
    let shell_phone = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&shell_phone).await.unwrap();
    let off = m.shell(80, 24, None).await.map(|(o, _, _)| o);
    assert_eq!(refused_code(off), ErrorCode::Forbidden);

    set_shell(&h, true);
    let control_phone = h.phone(Some(Permission::Control)).await;
    let c = h.connect(&control_phone).await.unwrap();
    assert_eq!(
        refused_code(c.shell(80, 24, None).await.map(|(o, _, _)| o)),
        ErrorCode::Forbidden
    );
    assert_eq!(refused_code(c.shells().await), ErrorCode::Forbidden);
    assert_eq!(h.audit_events("refused"), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_runs_commands_and_a_reattach_shows_the_screen() {
    let h = Harness::start().await;
    set_shell(&h, true);
    let phone = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&phone).await.unwrap();

    let (id, mut tx, mut rx) = open(&m, None).await;
    tx.input(b"echo pgpu-$((6*7))\n".to_vec()).await.unwrap();
    read_until(&mut rx, "pgpu-42").await;

    // Leave without closing, like switching apps.
    drop((tx, rx));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let list = m.shells().await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(!list[0].attached);

    let (opened, mut tx, mut rx) = m.shell(100, 30, Some(id)).await.unwrap();
    assert!(opened.reattached);
    read_until(&mut rx, "pgpu-42").await;
    tx.input(b"echo again-$((1+1))\n".to_vec()).await.unwrap();
    read_until(&mut rx, "again-2").await;

    tx.close().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(m.shells().await.unwrap().is_empty());
    assert_eq!(h.audit_events("shell_open"), 1);
    assert_eq!(h.audit_events("shell_close"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_reports_the_code() {
    let h = Harness::start().await;
    set_shell(&h, true);
    let phone = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&phone).await.unwrap();
    let (_, mut tx, mut rx) = open(&m, None).await;
    tx.input(b"exit 3\n".to_vec()).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, rx.next()).await {
            Ok(Ok(Some(ShellEvent::Exited(code)))) => {
                assert_eq!(code, Some(3));
                break;
            }
            Ok(Ok(Some(_))) => {}
            other => panic!("expected Exited, got {other:?}"),
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(m.shells().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_keeps_at_most_three_sessions_and_only_reattaches_its_own() {
    let h = Harness::start().await;
    set_shell(&h, true);
    let phone = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&phone).await.unwrap();
    let mut open_ones = Vec::new();
    for _ in 0..3 {
        open_ones.push(open(&m, None).await);
    }
    assert_eq!(
        refused_code(m.shell(80, 24, None).await.map(|(o, _, _)| o)),
        ErrorCode::Busy
    );

    let other = h.phone(Some(Permission::Shell)).await;
    let o = h.connect(&other).await.unwrap();
    let theirs = o
        .shell(80, 24, Some(open_ones[0].0))
        .await
        .map(|(o, _, _)| o);
    assert_eq!(refused_code(theirs), ErrorCode::NotFound);
    assert!(o.shells().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_the_terminal_off_closes_every_session() {
    let h = Harness::start().await;
    set_shell(&h, true);
    let phone = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&phone).await.unwrap();
    let (_, _tx, mut rx) = open(&m, None).await;
    set_shell(&h, false);
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.next().await {
                Ok(Some(ShellEvent::Output(_) | ShellEvent::Title(_))) => {}
                other => return other,
            }
        }
    })
    .await
    .expect("the session ends");
    assert!(
        matches!(&ended, Err(ClientError::Remote(e)) if e.code == ErrorCode::Forbidden),
        "{ended:?}"
    );
    assert_eq!(h.agent.shells.count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_shell_permission_closes_the_phones_sessions() {
    let h = Harness::start().await;
    set_shell(&h, true);
    let phone = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&phone).await.unwrap();
    let (_, _tx, _rx) = open(&m, None).await;
    let reply = handle(
        &h.agent,
        Request::SetDevicePermission {
            device: "test phone".into(),
            permission: Permission::Control,
        },
    );
    assert!(matches!(reply, Response::Device(_)), "{reply:?}");
    for _ in 0..50 {
        if h.agent.shells.count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the session should close within a few seconds");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_detached_session_closes_after_the_timeout() {
    let h = Harness::start().await;
    set_shell(&h, true);
    h.agent.shells.set_detach_timeout(Duration::from_secs(1));
    let phone = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&phone).await.unwrap();
    let (id, tx, rx) = open(&m, None).await;
    drop((tx, rx));
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(h.agent.shells.count(), 0);
    let gone = m.shell(80, 24, Some(id)).await.map(|(o, _, _)| o);
    assert_eq!(refused_code(gone), ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn keystrokes_echo_fast() {
    let h = Harness::start().await;
    set_shell(&h, true);
    let phone = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&phone).await.unwrap();
    let (_, mut tx, mut rx) = open(&m, None).await;
    tx.input(b"echo ready-$((2+3))\n".to_vec()).await.unwrap();
    read_until(&mut rx, "ready-5").await;

    let mut worst = Duration::ZERO;
    for c in "pocketgpu".chars() {
        let start = std::time::Instant::now();
        tx.input(c.to_string().into_bytes()).await.unwrap();
        read_until(&mut rx, &c.to_string()).await;
        worst = worst.max(start.elapsed());
    }
    println!("slowest echo: {worst:?}");
    // F16 allows 100 ms on a direct connection; on one machine it should be far less.
    assert!(worst < Duration::from_millis(100), "slowest echo {worst:?}");
}
