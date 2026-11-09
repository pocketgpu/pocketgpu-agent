//! F40 user story 3 over a real connection: `pgpu send` needs files on, pushes the phones
//! that have a push ticket with the machine's name and never the file's, and the Overview
//! holds the newest offers while `List` has them all.

mod harness;

use std::time::Duration;

use harness::Harness;
use harness::push::fake_push_server;
use pocketgpu::store::devices;
use pocketgpu_proto::Permission;
use pocketgpu_proto::files::{OVERVIEW_OFFERS, Root};
use pocketgpu_proto::local::{Request, Response};

async fn offer(h: &Harness, path: &std::path::Path) -> Response {
    let req = Request::Offer {
        path: path.to_str().unwrap().to_string(),
        to: None,
    };
    pocketgpu::files::local_request(&h.agent, req).await
}

fn file(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, b"weights").unwrap();
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn offers_need_files_on() {
    let h = Harness::start().await;
    let dir = tempfile::tempdir().unwrap();
    let _phone = h.phone(Some(Permission::Control)).await;
    match offer(&h, &file(&dir, "adapter.safetensors")).await {
        Response::Error(e) => assert!(e.contains("pgpu files on"), "{e}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.audit_events("file_offer"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn phones_with_a_ticket_get_a_push_without_the_file_name() {
    let h = Harness::start().await;
    let (url, mut alerts) = fake_push_server().await;
    {
        let mut cfg = h.agent.config.write().unwrap();
        cfg.files = true;
        cfg.push_url = url;
    }
    let pixel = h.phone(Some(Permission::Control)).await;
    let _desktop = h.phone(Some(Permission::Control)).await;
    {
        let db = h.agent.db();
        let id = pixel.key.public().to_string();
        db.execute(
            "UPDATE devices SET name = 'Pixel 8' WHERE device_id = ?1",
            [&id],
        )
        .unwrap();
        devices::set_push_ticket(&db, &id, "0123456789abcdef0123456789abcdef").unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    match offer(&h, &file(&dir, "adapter.safetensors")).await {
        Response::Offered { to, pushed, .. } => {
            assert_eq!(to.len(), 2);
            assert_eq!(pushed, vec!["Pixel 8".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    let (valid, alert) = tokio::time::timeout(Duration::from_secs(10), alerts.recv())
        .await
        .expect("a push within 10 seconds")
        .unwrap();
    assert!(valid, "the push must be signed with the agent's key");
    assert_eq!(alert.tickets, vec!["0123456789abcdef0123456789abcdef"]);
    assert_eq!(
        alert.title,
        format!("{} has a file for you", h.agent.name())
    );
    assert!(!alert.title.contains("adapter") && !alert.body.contains("adapter"));
    assert!(alert.link.unwrap().ends_with("/files"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_overview_holds_the_newest_offers() {
    let h = Harness::start().await;
    h.agent.config.write().unwrap().files = true;
    let phone = h.phone(Some(Permission::Control)).await;
    let control = h.connect(&phone).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "out.png");
    let many = OVERVIEW_OFFERS + 10;
    for _ in 0..many {
        assert!(matches!(offer(&h, &path).await, Response::Offered { .. }));
    }
    let overview = control.files_overview().await.unwrap();
    assert_eq!(overview.offers.len(), OVERVIEW_OFFERS);
    let (all, next) = control.files_list(Root::Offers, "", 0).await.unwrap();
    assert_eq!((all.len(), next), (many, None));
}
