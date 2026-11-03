//! F37: power requests from phones are refused unless turned on at the machine, and need
//! Control. (The countdown and the command are tested in `power::tests` with a fake runner,
//! so nothing here ever restarts the test machine.)

mod harness;

use harness::Harness;
use pocketgpu::ipc::handle;
use pocketgpu_client::ClientError;
use pocketgpu_proto::local::{Request as Local, Response as Reply};
use pocketgpu_proto::msg::control::{PowerKind, Request, Response};
use pocketgpu_proto::{ErrorCode, Permission};

#[tokio::test]
async fn off_by_default_and_view_phones_may_only_look() {
    let h = Harness::start().await;
    let viewer = h.phone(Some(Permission::View)).await;
    let control = h.phone(Some(Permission::Control)).await;
    let v = h.connect(&viewer).await.unwrap();
    let c = h.connect(&control).await.unwrap();

    let Response::Power(state) = v.control(Request::GetPower).await.unwrap() else {
        panic!("expected the power state");
    };
    assert!(!state.enabled);

    let res = v
        .control(Request::PowerAction {
            action: PowerKind::Restart,
        })
        .await;
    assert!(matches!(res, Err(ClientError::Remote(e)) if e.code == ErrorCode::Forbidden));

    let res = c
        .control(Request::PowerAction {
            action: PowerKind::ShutDown,
        })
        .await;
    assert!(
        matches!(&res, Err(ClientError::Remote(e)) if e.message.contains("pgpu power on")),
        "{res:?}"
    );
    assert!(h.agent.power.pending().is_none());

    // Turned on here; cancel with nothing pending says so.
    assert!(matches!(handle(&h.agent, Local::SetPower { on: true }), Reply::Power(s) if s.enabled));
    assert!(matches!(
        handle(&h.agent, Local::CancelPower),
        Reply::Error(_)
    ));
    assert!(
        matches!(handle(&h.agent, Local::SetPower { on: false }), Reply::Power(s) if !s.enabled)
    );
}
