//! F44 end to end: a device with Shell lends the machine; the device using the invite pairs
//! at once with the invite's level, the invite works once, a guest can't invite others, the
//! owner gives it more time, and when the time is up the machine refuses it and removes it.

mod harness;

use harness::Harness;
use pocketgpu_client::{ClientError, PhoneInfo};
use pocketgpu_proto::msg::pair;
use pocketgpu_proto::pairing::PairInput;
use pocketgpu_proto::{ErrorCode, Permission, Platform};

fn info(name: &str) -> PhoneInfo {
    PhoneInfo {
        device_name: name.into(),
        platform: Platform::Android,
        push_ticket: None,
    }
}

fn refusal<T: std::fmt::Debug>(r: Result<T, ClientError>) -> ErrorCode {
    match r {
        Err(ClientError::Remote(e)) => e.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_joins_at_once_and_its_access_ends() {
    let h = Harness::start().await;
    let owner = h.phone(Some(Permission::Shell)).await;
    let owner_m = h.connect(&owner).await.unwrap();
    let viewer = h.phone(Some(Permission::Control)).await;
    let viewer_m = h.connect(&viewer).await.unwrap();

    // Only Shell makes invites.
    assert_eq!(
        refusal(viewer_m.create_invite(Permission::View, 60).await),
        ErrorCode::Forbidden
    );
    assert_eq!(
        refusal(owner_m.create_invite(Permission::Control, 0).await),
        ErrorCode::Invalid
    );
    let invite = owner_m
        .create_invite(Permission::Control, 8 * 60)
        .await
        .unwrap();
    assert!(invite.uri.contains("&guest=1&"), "{}", invite.uri);
    assert!(invite.uri.contains("&perm=control"), "{}", invite.uri);
    let PairInput::Qr(qr) = PairInput::parse(&invite.uri).unwrap() else {
        panic!("the invite should be a QR link");
    };

    // The guest pairs at once: Pending, then Approved, with no question at the machine.
    let guest = h.phone(None).await;
    let mut pairing = guest
        .client
        .pair_at(
            h.router.endpoint().addr(),
            qr.code,
            None,
            info("Alex's Pixel"),
        )
        .await
        .unwrap();
    assert!(matches!(
        pairing.next().await.unwrap(),
        pair::Response::Pending { .. }
    ));
    assert!(matches!(
        pairing.next().await.unwrap(),
        pair::Response::Approved {
            permission: Permission::Control,
            ..
        }
    ));
    let guest_key = guest.key.public();
    assert_eq!(h.agent.permission_of(&guest_key), Some(Permission::Control));

    // Once only.
    let second = h.phone(None).await;
    let again = second
        .client
        .pair_at(h.router.endpoint().addr(), qr.code, None, info("Other"))
        .await;
    let refused = match again {
        Err(_) => true,
        Ok(mut p) => p.next().await.is_err(),
    };
    assert!(refused, "the invite worked twice");

    // The owner sees it with its end; the guest can't invite others or give itself time.
    let guests = owner_m.guests().await.unwrap();
    assert_eq!(guests.len(), 1);
    assert_eq!(guests[0].name, "Alex's Pixel");
    let first_end = guests[0].ends_ms;
    assert!((first_end - invite.ends_ms).abs() < 1000);
    let guest_m = h.connect(&guest).await.unwrap();
    assert_eq!(
        refusal(guest_m.create_invite(Permission::View, 60).await),
        ErrorCode::Forbidden
    );
    let more = owner_m.extend_guest(&guests[0].id, 60).await.unwrap();
    assert_eq!(more[0].ends_ms, first_end + 3_600_000);

    // Time is up: refused at once, then removed by the sweeper with an audit entry.
    pocketgpu::store::devices::set_expiry(&h.agent.db(), &guests[0].id, Some(1)).unwrap();
    assert_eq!(h.agent.permission_of(&guest_key), None);
    assert!(guest_m.hello("test").await.is_err());
    assert_eq!(pocketgpu::pair::guest::sweep(&h.agent), 1);
    assert!(owner_m.guests().await.unwrap().is_empty());
    let ended = pocketgpu::audit::since(&h.agent.db(), 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.event == "revoke" && e.result == pocketgpu::pair::guest::GUEST_ENDED)
        .count();
    assert_eq!(ended, 1);
}
