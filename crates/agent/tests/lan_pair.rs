//! Pairing from the same Wi-Fi (F47) end to end: `pgpu pair` session ↔ agent ↔ phone, over
//! loopback, with the phone holding what a Wi-Fi record would have shown.

mod harness;

use harness::Harness;
use pocketgpu_client::PhoneInfo;
use pocketgpu_client::lan::Found;
use pocketgpu_proto::lan::Os;
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::msg::{control, pair};
use pocketgpu_proto::{Permission, Platform, read_frame, write_frame};

fn info() -> PhoneInfo {
    PhoneInfo {
        device_name: "Pixel 9".into(),
        platform: Platform::Android,
        push_ticket: None,
    }
}

/// What Found on this Wi-Fi would hold for this agent: its id and its loopback address.
fn found(h: &Harness) -> Found {
    let port = h
        .router
        .endpoint()
        .bound_sockets()
        .into_iter()
        .find(|s| s.is_ipv4())
        .expect("an IPv4 socket")
        .port();
    Found {
        fullname: "test._pgpu._udp.local.".into(),
        agent_id: h.agent.identity.agent_id(),
        name: h.agent.name(),
        os: Os::this(),
        addrs: vec![([127, 0, 0, 1], port).into()],
    }
}

#[tokio::test]
async fn a_phone_found_on_the_wifi_pairs_after_a_yes() {
    let h = Harness::start().await;
    let (cli, agent_side) = tokio::io::duplex(64 * 1024);
    let (ar, aw) = tokio::io::split(agent_side);
    tokio::spawn(pocketgpu::pair::session(h.agent.clone(), ar, aw));
    let (mut r, mut w) = tokio::io::split(cli);
    let Some(Response::PairCode(_)) = read_frame(&mut r).await.unwrap() else {
        panic!("expected a pairing code first");
    };

    let phone = h.phone(None).await;
    let mut pairing = phone.client.pair_lan(&found(&h), info()).await.unwrap();
    assert!(matches!(
        pairing.next().await.unwrap(),
        pair::Response::Pending { .. }
    ));

    let Some(Response::PairAsk(ask)) = read_frame(&mut r).await.unwrap() else {
        panic!("expected PairAsk");
    };
    assert!(
        ask.from_lan,
        "the computer must know it came from the Wi-Fi"
    );
    assert_eq!(ask.device_name, "Pixel 9");
    write_frame(
        &mut w,
        &Request::PairDecision {
            permission: Some(Permission::View),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        pairing.next().await.unwrap(),
        pair::Response::Approved {
            permission: Permission::View,
            relays: vec![],
        }
    );
    assert_eq!(
        h.agent.permission_of(&phone.key.public()),
        Some(Permission::View)
    );
    let result: String = h
        .agent
        .db()
        .query_row(
            "SELECT result FROM audit WHERE event = 'pair' ORDER BY audit_id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(result, "view · on this Wi-Fi");
}

#[tokio::test]
async fn nothing_is_heard_without_a_pgpu_pair_session() {
    let h = Harness::start().await;
    let phone = h.phone(None).await;
    let result = match phone.client.pair_lan(&found(&h), info()).await {
        Ok(mut p) => p.next().await.map(|_| ()),
        Err(e) => Err(e),
    };
    assert!(
        result.is_err(),
        "pairing from the Wi-Fi needs `pgpu pair` running"
    );
    assert_eq!(h.agent.permission_of(&phone.key.public()), None);
}

#[tokio::test]
async fn nothing_is_heard_when_finding_on_the_wifi_is_off() {
    let h = Harness::start().await;
    h.agent.config.write().unwrap().lan = false;
    let (cli, agent_side) = tokio::io::duplex(64 * 1024);
    let (ar, aw) = tokio::io::split(agent_side);
    tokio::spawn(pocketgpu::pair::session(h.agent.clone(), ar, aw));
    let (mut r, _w) = tokio::io::split(cli);
    let _ = read_frame::<_, Response>(&mut r).await.unwrap();

    let phone = h.phone(None).await;
    let result = match phone.client.pair_lan(&found(&h), info()).await {
        Ok(mut p) => p.next().await.map(|_| ()),
        Err(e) => Err(e),
    };
    assert!(result.is_err());
    assert_eq!(h.agent.permission_of(&phone.key.public()), None);
}

#[tokio::test]
async fn a_paired_phone_gets_the_lan_key_and_whether_finding_is_on() {
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    let first = machine.control(control::Request::GetLanKey).await.unwrap();
    let control::Response::LanKey { key, on } = first else {
        panic!("expected LanKey, got {first:?}");
    };
    assert!(on, "on by default");
    assert_ne!(key, [0u8; 32]);

    h.agent.config.write().unwrap().lan = false;
    let again = machine.control(control::Request::GetLanKey).await.unwrap();
    assert_eq!(
        again,
        control::Response::LanKey { key, on: false },
        "same key, now off"
    );
}

#[tokio::test]
async fn an_unpaired_phone_never_gets_the_lan_key() {
    let h = Harness::start().await;
    let stranger = h.phone(None).await;
    let result = match h.connect(&stranger).await {
        Ok(m) => m.control(control::Request::GetLanKey).await.map(|_| ()),
        Err(e) => Err(e),
    };
    assert!(result.is_err());
}
