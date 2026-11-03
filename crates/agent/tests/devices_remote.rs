//! F35: phones list and revoke a machine's devices; the tray's Connected list; the notice
//! level, only changeable here.

mod harness;

use harness::Harness;
use pocketgpu::ipc::handle;
use pocketgpu_client::ClientError;
use pocketgpu_proto::local::{Activity, NoticeLevel, Request as Local, Response as Reply};
use pocketgpu_proto::msg::control::{Request, Response};
use pocketgpu_proto::{ErrorCode, Permission};

#[tokio::test]
async fn a_shell_phone_lists_and_revokes_another_device() {
    let h = Harness::start().await;
    let owner = h.phone(Some(Permission::Shell)).await;
    let stranger = h.phone(Some(Permission::Control)).await;
    let machine = h.connect(&owner).await.unwrap();
    let other = h.connect(&stranger).await.unwrap();

    let Response::Devices(list) = machine.control(Request::ListDevices).await.unwrap() else {
        panic!("expected devices");
    };
    assert_eq!(list.len(), 2);
    assert_eq!(list.iter().filter(|d| d.you).count(), 1);
    let target = list.iter().find(|d| !d.you).unwrap().id.clone();
    let me = list.iter().find(|d| d.you).unwrap().id.clone();

    // A Control phone may list but not revoke.
    assert!(matches!(
        other.control(Request::ListDevices).await,
        Ok(Response::Devices(_))
    ));
    let res = other
        .control(Request::RevokeDevice { device: me.clone() })
        .await;
    assert!(matches!(res, Err(ClientError::Remote(e)) if e.code == ErrorCode::Forbidden));

    // Not itself.
    let res = machine.control(Request::RevokeDevice { device: me }).await;
    assert!(matches!(res, Err(ClientError::Remote(e)) if e.code == ErrorCode::Forbidden));

    let res = machine
        .control(Request::RevokeDevice {
            device: target.clone(),
        })
        .await;
    assert!(matches!(res, Ok(Response::Revoked { device }) if device == target));
    let Response::Devices(list) = machine.control(Request::ListDevices).await.unwrap() else {
        panic!("expected devices");
    };
    assert_eq!(list.len(), 1);
    assert_eq!(h.audit_events("revoke"), 1);
}

#[tokio::test]
async fn the_computers_own_app_cant_be_revoked_from_a_phone() {
    let h = Harness::start().await;
    let owner = h.phone(Some(Permission::Shell)).await;
    let machine = h.connect(&owner).await.unwrap();
    let app = iroh::SecretKey::generate().public().to_string();
    let reply = handle(
        &h.agent,
        Local::PairLocal {
            device: app.clone(),
            name: "PocketGPU on this PC".into(),
        },
    );
    assert!(matches!(reply, Reply::PairedLocal { .. }));
    let Response::Devices(list) = machine.control(Request::ListDevices).await.unwrap() else {
        panic!("expected devices");
    };
    assert!(list.iter().any(|d| d.id == app && d.this_pc));
    let res = machine.control(Request::RevokeDevice { device: app }).await;
    assert!(matches!(res, Err(ClientError::Remote(e)) if e.code == ErrorCode::Forbidden));
}

#[tokio::test]
async fn connected_lists_phones_and_the_level_changes_only_here() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    machine.hello("test").await.unwrap();

    let Reply::Connected(list) = handle(&h.agent, Local::Connected) else {
        panic!("expected connected devices");
    };
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].activity, Activity::Watching);

    assert!(matches!(
        handle(&h.agent, Local::GetNotices),
        Reply::Notices(NoticeLevel::All)
    ));
    assert!(matches!(
        handle(&h.agent, Local::SetNotices(NoticeLevel::Fewer)),
        Reply::Notices(NoticeLevel::Fewer)
    ));
    assert!(matches!(
        handle(&h.agent, Local::GetNotices),
        Reply::Notices(NoticeLevel::Fewer)
    ));
}
