//! F32 safety for `pgpu send`: an AI assistant here can run it too, so the agent itself
//! refuses keys and secrets, and offers reach the owner's devices only, never guests (F44).

use crate::harness;

use std::path::PathBuf;

use harness::Harness;
use pocketgpu::store::devices;
use pocketgpu_proto::Permission;
use pocketgpu_proto::local::{Request, Response};

struct Setup {
    h: Harness,
    dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let h = Harness::start().await;
    let dir = tempfile::Builder::new()
        .prefix("pgpu-safe-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    {
        let mut cfg = h.agent.config.write().unwrap();
        cfg.files = true;
        cfg.files_inbox = Some(dir.path().join("Inbox"));
    }
    Setup { h, dir }
}

impl Setup {
    fn file(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, b"x").unwrap();
        path
    }

    async fn offer(&self, path: &std::path::Path, to: Option<&str>) -> Response {
        let req = Request::Offer {
            path: path.to_str().unwrap().into(),
            to: to.map(str::to_string),
        };
        pocketgpu::files::local_request(&self.h.agent, req).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_and_secrets_are_never_offered() {
    let s = setup().await;
    let _phone = s.h.phone(Some(Permission::Control)).await;
    for name in [
        ".env",
        ".env.local",
        "server.key",
        "cert.pem",
        "id_ed25519",
        "credentials",
    ] {
        let path = s.file(name);
        match s.offer(&path, None).await {
            Response::Error(e) => {
                assert_eq!(
                    e,
                    format!("PocketGPU doesn't send keys or secrets: {}", path.display())
                );
            }
            other => panic!("{name}: {other:?}"),
        }
    }
    // A harmless name that leads to a key is caught through the link.
    #[cfg(unix)]
    {
        let key = s.file("id_rsa");
        let link = s.dir.path().join("notes.txt");
        std::os::unix::fs::symlink(&key, &link).unwrap();
        assert!(matches!(s.offer(&link, None).await, Response::Error(e) if e.contains("secrets")));
    }
    let ok = s.file("model.safetensors");
    assert!(matches!(s.offer(&ok, None).await, Response::Offered { .. }));
    let count: u32 =
        s.h.agent
            .db()
            .query_row("SELECT count(*) FROM offers", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 1, "nothing refused was kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn offers_skip_guests_and_refuse_naming_one() {
    let s = setup().await;
    let owner = s.h.phone(Some(Permission::Control)).await;
    let guest = s.h.phone(Some(Permission::Control)).await;
    {
        let db = s.h.agent.db();
        let rename = "UPDATE devices SET name = ?1 WHERE device_id = ?2";
        db.execute(rename, ["Pixel", &owner.key.public().to_string()])
            .unwrap();
        db.execute(rename, ["Visitor", &guest.key.public().to_string()])
            .unwrap();
        let ends = pocketgpu::store::db::now_ms() + 3_600_000;
        devices::set_expiry(&db, &guest.key.public().to_string(), Some(ends)).unwrap();
    }
    let path = s.file("out.png");
    match s.offer(&path, None).await {
        Response::Offered { to, .. } => assert_eq!(to, vec!["Pixel".to_string()]),
        other => panic!("{other:?}"),
    }
    match s.offer(&path, Some("Visitor")).await {
        Response::Error(e) => assert!(e.contains("guest"), "{e}"),
        other => panic!("{other:?}"),
    }
    let g = s.h.connect(&guest).await.unwrap();
    assert!(g.files_overview().await.unwrap().offers.is_empty());
    let o = s.h.connect(&owner).await.unwrap();
    assert_eq!(o.files_overview().await.unwrap().offers.len(), 1);
}
