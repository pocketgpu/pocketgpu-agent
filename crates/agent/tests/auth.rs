//! Security gates (SC-009): unpaired keys get nothing, and no request succeeds above the
//! phone's permission. Every refusal is audited.

mod harness;

use std::time::Duration;

use harness::Harness;
use pocketgpu_client::ClientError;
use pocketgpu_proto::msg::control::{AlertRule, NewAction, Range, Request};
use pocketgpu_proto::{ErrorCode, Permission};

fn every_control_request() -> Vec<Request> {
    let action = NewAction {
        name: "x".into(),
        command: "true".into(),
        icon: "zap".into(),
        risky: false,
        timeout_s: None,
    };
    vec![
        Request::Hello {
            app_version: "test".into(),
            proto: 1,
        },
        Request::GetHistory {
            range: Range::H24,
            gpu: 0,
        },
        Request::ListProcesses,
        Request::KillProcess { pid: 1 },
        Request::ListActions,
        Request::RunAction { id: 1 },
        Request::CreateAction(action.clone()),
        Request::EditAction { id: 1, action },
        Request::DeleteAction { id: 1 },
        Request::GetAlertSettings,
        Request::SetAlertSettings {
            rules: Vec::<AlertRule>::new(),
        },
        Request::SetPushTicket { ticket: "t".into() },
        Request::Status {
            app_version: "test".into(),
            proto: 1,
        },
        // Last: it removes the phone.
        Request::Unpair,
    ]
}

#[tokio::test]
async fn unpaired_device_gets_nothing() {
    let h = Harness::start().await;
    let phone = h.phone(None).await;
    // The handshake may complete, but the agent closes before reading any message.
    if let Ok(machine) = h.connect(&phone).await {
        let res = tokio::time::timeout(Duration::from_secs(5), machine.hello("test"))
            .await
            .expect("should not hang");
        assert!(res.is_err(), "an unpaired device must not get a reply");
    }
}

#[tokio::test]
async fn requests_above_permission_are_refused_and_audited() {
    let h = Harness::start().await;
    // With terminal access on, permission alone decides (actions need it too).
    pocketgpu::ipc::handle(
        &h.agent,
        pocketgpu_proto::local::Request::SetShell { on: true },
    );
    for have in [Permission::View, Permission::Control, Permission::Shell] {
        let phone = h.phone(Some(have)).await;
        let machine = h.connect(&phone).await.unwrap();
        for req in every_control_request() {
            let needed = req.min_permission();
            let refused_before = h.audit_events("refused");
            let res = machine.control(req.clone()).await;
            let forbidden =
                matches!(&res, Err(ClientError::Remote(e)) if e.code == ErrorCode::Forbidden);
            assert_eq!(forbidden, needed > have, "{req:?} with {have:?}: {res:?}");
            let refused_after = h.audit_events("refused");
            assert_eq!(
                refused_after - refused_before,
                usize::from(needed > have),
                "{req:?} audit"
            );
        }
    }
}

#[tokio::test]
async fn paired_view_phone_sees_machine_and_live_stats() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    let info = machine.hello("test").await.unwrap();
    assert_eq!(info.gpus.len(), 2);
    assert_eq!(info.permission, Permission::View);

    let mut stream = machine.subscribe(Duration::from_secs(1)).await.unwrap();
    let sample = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(sample.gpus.len(), 2);
}

#[tokio::test]
async fn status_answers_everything_in_one_reply() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    // The first sample arrives within a second or two of the agent starting.
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = machine.status("test").await.unwrap();
            if s.sample.is_some() {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(status.info.gpus.len(), 2);
    assert_eq!(status.info.permission, Permission::View);
    assert_eq!(status.sample.unwrap().gpus.len(), 2);
    assert!(status.running.is_none());
}
