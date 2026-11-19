//! Find on this Wi-Fi (F47) on the real network: the agent's record goes out over multicast DNS
//! and the phone's browser finds it, then pairs over the computer's Wi-Fi address. Needs a
//! network that allows multicast, so it is ignored by default:
//! `cargo test -p pocketgpu --test it lan_wifi -- --ignored --nocapture`.

use crate::harness;

use std::time::Duration;

use harness::Harness;
use pocketgpu_client::PhoneInfo;
use pocketgpu_client::lan::{Seen, browse};
use pocketgpu_proto::lan::{hour_of, tag_matches};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::msg::pair;
use pocketgpu_proto::{Permission, Platform, read_frame, write_frame};
use tokio::sync::mpsc::Receiver;

fn now_hour() -> u64 {
    hour_of(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
}

async fn next_matching<T>(rx: &mut Receiver<Seen>, mut f: impl FnMut(Seen) -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let seen = rx.recv().await.expect("browser stopped");
            if let Some(t) = f(seen) {
                return t;
            }
        }
    })
    .await
    .expect("nothing seen on the network within 20 s")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real network with multicast"]
async fn found_and_paired_over_the_real_wifi() {
    let h = Harness::start().await;
    let key = pocketgpu::lan::load_or_make_key(&h.agent.paths).unwrap();
    let sockets = h.router.endpoint().bound_sockets();
    let announcer = tokio::spawn(pocketgpu::lan::announce(h.agent.clone(), sockets));
    let (_browser, mut rx) = browse().unwrap();

    // 1. No session: only the hourly tag, which this agent's key recognises.
    let quiet = next_matching(&mut rx, |s| match s {
        Seen::Quiet { tag, addrs, .. } if tag_matches(&key, &tag, now_hour()) => Some(addrs),
        _ => None,
    })
    .await;
    println!("quiet record seen at {quiet:?}");

    // 2. `pgpu pair` opens: the name shows, with the agent's own key.
    let (cli, agent_side) = tokio::io::duplex(64 * 1024);
    let (ar, aw) = tokio::io::split(agent_side);
    tokio::spawn(pocketgpu::pair::session(h.agent.clone(), ar, aw));
    let (mut r, mut w) = tokio::io::split(cli);
    let Some(Response::PairCode(_)) = read_frame(&mut r).await.unwrap() else {
        panic!("expected a pairing code first");
    };
    let id = h.agent.identity.agent_id();
    let found = next_matching(&mut rx, |s| match s {
        // The record comes again as more of its addresses arrive; wait for the Wi-Fi one.
        Seen::Waiting(f) if f.agent_id == id && f.addrs.iter().any(|a| a.is_ipv4()) => Some(f),
        _ => None,
    })
    .await;
    println!(
        "waiting record: {} ({:?}) at {:?}",
        found.name, found.os, found.addrs
    );
    assert!(found.addrs.iter().all(|a| !a.ip().is_loopback()));

    // 3. The phone asks over the Wi-Fi address only; the computer says yes.
    let phone = h.phone(None).await;
    let info = PhoneInfo {
        device_name: "Wi-Fi test phone".into(),
        platform: Platform::Ios,
        push_ticket: None,
    };
    let mut pairing = phone.client.pair_lan(&found, info).await.unwrap();
    assert!(matches!(
        pairing.next().await.unwrap(),
        pair::Response::Pending { .. }
    ));
    let Some(Response::PairAsk(ask)) = read_frame(&mut r).await.unwrap() else {
        panic!("expected PairAsk");
    };
    assert!(ask.from_lan);
    write_frame(
        &mut w,
        &Request::PairDecision {
            permission: Some(Permission::View),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        pairing.next().await.unwrap(),
        pair::Response::Approved { .. }
    ));
    assert_eq!(
        h.agent.permission_of(&phone.key.public()),
        Some(Permission::View)
    );
    println!("paired over {:?}", found.addrs);

    // 4. The session ends: the name goes away again.
    drop((r, w));
    next_matching(&mut rx, |s| match s {
        Seen::Lost { fullname } if fullname == found.fullname => Some(()),
        _ => None,
    })
    .await;
    println!("waiting record gone after the session");
    announcer.abort();
}
