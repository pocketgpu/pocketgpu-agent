//! F20: alert channels, saved in config.toml and sent straight from the agent.

mod harness;

use std::sync::{Arc, Mutex};

use harness::Harness;
use pocketgpu::alerts::channels::{Alert, send_all};
use pocketgpu::ipc::handle;
use pocketgpu::store::config::Config;
use pocketgpu_proto::local::{Channel, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A webhook receiver that keeps every request it gets.
async fn receiver() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook?key=secret", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let keep = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = vec![0u8; 16 * 1024];
            let n = s.read(&mut buf).await.unwrap_or(0);
            keep.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        }
    });
    (url, seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn channels_are_saved_masked_and_receive_alerts() {
    let h = Harness::start().await;
    let (url, seen) = receiver().await;

    let bad = handle(
        &h.agent,
        Request::AddChannel(Channel::Ntfy {
            topic: "has space".into(),
        }),
    );
    assert!(matches!(bad, Response::Error(_)), "{bad:?}");
    let reply = handle(
        &h.agent,
        Request::AddChannel(Channel::Webhook { url: url.clone() }),
    );
    let Response::Channels(list) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(list.len(), 1);
    assert!(
        !list[0].contains("secret"),
        "the listing hides the URL's secret: {}",
        list[0]
    );
    let again = handle(
        &h.agent,
        Request::AddChannel(Channel::Webhook { url: url.clone() }),
    );
    assert!(
        matches!(again, Response::Error(_)),
        "duplicates are refused"
    );

    let saved = Config::load(&h.agent.paths.config_file()).unwrap();
    assert_eq!(saved.notify, vec![Channel::Webhook { url }]);

    let alert = Alert {
        title: "train.py finished · rig-1".into(),
        body: "Job 12 · exit 0 after 2h 14m".into(),
        kind: "job_end",
        machine: "rig-1".into(),
        job: Some(12),
    };
    let results = send_all(&h.agent, &alert).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1, None, "{results:?}");
    let got = seen.lock().unwrap().join("\n");
    assert!(got.contains("\"kind\":\"job_end\""), "{got}");
    assert!(got.contains("train.py finished"), "{got}");

    let removed = handle(&h.agent, Request::RemoveChannel { number: 1 });
    assert!(
        matches!(removed, Response::Channels(ref l) if l.is_empty()),
        "{removed:?}"
    );
    let missing = handle(&h.agent, Request::RemoveChannel { number: 1 });
    assert!(matches!(missing, Response::Error(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_channel_reports_why() {
    let h = Harness::start().await;
    let added = handle(
        &h.agent,
        Request::AddChannel(Channel::Webhook {
            url: "http://127.0.0.1:9/hook".into(),
        }),
    );
    assert!(matches!(added, Response::Channels(_)));
    let alert = Alert {
        title: "t".into(),
        body: "b".into(),
        kind: "test",
        machine: "m".into(),
        job: None,
    };
    let results = send_all(&h.agent, &alert).await;
    assert!(
        results[0]
            .1
            .as_deref()
            .is_some_and(|e| e.contains("could not reach")),
        "{results:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn custom_relays_are_checked_and_saved() {
    let h = Harness::start().await;
    let bad = handle(
        &h.agent,
        Request::SetRelays(vec!["http://relay.example.com".into()]),
    );
    assert!(
        matches!(bad, Response::Error(ref e) if e.contains("https://")),
        "{bad:?}"
    );
    let urls = vec![
        "https://relay.rig.home".into(),
        "https://relay.rig.home".into(),
    ];
    let set = handle(&h.agent, Request::SetRelays(urls));
    assert!(
        matches!(set, Response::Relays(ref l) if l == &["https://relay.rig.home"]),
        "{set:?}"
    );
    let saved = Config::load(&h.agent.paths.config_file()).unwrap();
    assert_eq!(saved.relays, vec!["https://relay.rig.home".to_string()]);
    let cleared = handle(&h.agent, Request::SetRelays(vec![]));
    assert!(matches!(cleared, Response::Relays(ref l) if l.is_empty()));
    assert_eq!(h.audit_events("relay_change"), 2);
}

#[tokio::test]
async fn open_apps_hear_alerts_as_they_fire() {
    let h = Harness::start().await;
    let phone = h.phone(Some(pocketgpu_proto::Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    let mut events = machine.events().await.unwrap();
    // The subscription is registered once the agent read it; retry until the alert arrives.
    let alert = Alert {
        title: "train.py finished · rig-1".into(),
        body: "Job 12 · exit 0".into(),
        kind: "job_end",
        machine: "rig-1".into(),
        job: Some(12),
    };
    let got = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            send_all(&h.agent, &alert).await;
            if let Ok(next) =
                tokio::time::timeout(std::time::Duration::from_millis(200), events.next()).await
            {
                return next.unwrap().unwrap();
            }
        }
    })
    .await
    .expect("an alert arrives");
    assert_eq!(got.title, alert.title);
    assert_eq!(got.kind, "job_end");
    assert_eq!(got.job, Some(12));
}
