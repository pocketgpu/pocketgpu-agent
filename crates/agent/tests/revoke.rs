//! Devices and permissions (F03): revoking disconnects at once, permission changes apply to
//! open connections, and a phone can unpair itself.

mod harness;

use std::time::{Duration, Instant};

use harness::Harness;
use pocketgpu::ipc::handle;
use pocketgpu_client::ClientError;
use pocketgpu_proto::local::{Request as Local, Response as Reply};
use pocketgpu_proto::msg::control::Request;
use pocketgpu_proto::{ErrorCode, Permission};

#[tokio::test]
async fn revoking_a_connected_phone_drops_its_stream_within_a_second() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::Control)).await;
    let machine = h.connect(&phone).await.unwrap();
    let mut stream = machine.subscribe(Duration::from_millis(200)).await.unwrap();
    // Let a sample arrive, so the stream is really live.
    h.agent.sampler.take(None);
    tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("a sample arrives")
        .unwrap();

    let Reply::Devices(list) = handle(&h.agent, Local::ListDevices) else {
        panic!("expected devices");
    };
    assert_eq!(list.len(), 1);
    assert!(list[0].connected);

    let start = Instant::now();
    let reply = handle(
        &h.agent,
        Local::RevokeDevice {
            device: "test phone".into(),
        },
    );
    assert!(matches!(reply, Reply::Device(d) if d.name == "Test phone"));

    let ended = tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = stream.next().await {}
    })
    .await;
    assert!(ended.is_ok(), "the stream should end within 1 s");
    assert!(start.elapsed() < Duration::from_secs(1));

    // A reconnect is refused: the agent closes before answering anything.
    if let Ok(again) = h.connect(&phone).await {
        let res = tokio::time::timeout(Duration::from_secs(5), again.hello("test")).await;
        assert!(
            matches!(res, Ok(Err(_))),
            "a revoked phone must get no reply: {res:?}"
        );
    }
    assert_eq!(h.audit_events("revoke"), 1);
}

#[tokio::test]
async fn permission_changes_apply_to_open_connections() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::Control)).await;
    let machine = h.connect(&phone).await.unwrap();
    machine.control(Request::ListProcesses).await.unwrap();

    let reply = handle(
        &h.agent,
        Local::SetDevicePermission {
            device: phone.key.public().to_string()[..10].into(),
            permission: Permission::View,
        },
    );
    assert!(matches!(reply, Reply::Device(d) if d.permission == Permission::View));

    // Same connection, next request: refused at the new level.
    let res = machine.control(Request::ListProcesses).await;
    assert!(matches!(res, Err(ClientError::Remote(e)) if e.code == ErrorCode::Forbidden));
    let info = machine.hello("test").await.unwrap();
    assert_eq!(info.permission, Permission::View);
    assert_eq!(h.audit_events("permission_change"), 1);
}

#[tokio::test]
async fn a_phone_can_unpair_itself() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    machine.control(Request::Unpair).await.unwrap();
    assert!(matches!(handle(&h.agent, Local::ListDevices), Reply::Devices(l) if l.is_empty()));
    let res = tokio::time::timeout(Duration::from_secs(5), machine.hello("test")).await;
    assert!(
        matches!(res, Ok(Err(_))),
        "an unpaired phone gets nothing more: {res:?}"
    );
    assert_eq!(h.audit_events("revoke"), 1);
}

#[tokio::test]
async fn unknown_or_ambiguous_devices_are_explained() {
    let h = Harness::start().await;
    h.phone(Some(Permission::View)).await;
    h.phone(Some(Permission::View)).await;
    let reply = handle(
        &h.agent,
        Local::RevokeDevice {
            device: "Test phone".into(),
        },
    );
    assert!(
        matches!(&reply, Reply::Error(e) if e.contains("2 phones match")),
        "{reply:?}"
    );
    let reply = handle(
        &h.agent,
        Local::RevokeDevice {
            device: "ipad".into(),
        },
    );
    assert!(
        matches!(&reply, Reply::Error(e) if e.contains("no paired phone")),
        "{reply:?}"
    );
}
