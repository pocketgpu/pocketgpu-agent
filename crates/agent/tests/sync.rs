//! F43 over a real connection: a shared folder kept on this device. The first round gets
//! every file in every folder, the next finds nothing new, a changed file comes again, a
//! file changed here is kept beside the new one, and a cut-off download carries on.

mod harness;

use harness::Harness;
use pocketgpu_client::Machine;
use pocketgpu_client::sync::{Known, PARTIAL, Seen, needed};
use pocketgpu_proto::Permission;
use pocketgpu_proto::files::Root;
use pocketgpu_proto::local::{Request, Response};

async fn local(h: &Harness, req: Request) -> Response {
    pocketgpu::files::local_request(&h.agent, req).await
}

/// One round: everything needed, into `dest`, remembered in `known`. The paths that came.
async fn round(m: &Machine, dest: &std::path::Path, known: &mut Known) -> Vec<String> {
    let remote = m.sync_scan(Root::Shared(0)).await.unwrap();
    let mut got = Vec::new();
    for f in needed(&remote, known) {
        let landed = known.landed(&f.path);
        m.sync_get(Root::Shared(0), &f, dest, landed, |_, _| {})
            .await
            .unwrap();
        known.0.insert(
            f.path.clone(),
            Seen {
                size: f.size,
                modified_ms: f.modified_ms,
            },
        );
        got.push(f.path);
    }
    got.sort();
    got
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_folder_stays_in_sync_here() {
    let h = Harness::start().await;
    let dir = tempfile::Builder::new()
        .prefix("pgpu-sync-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let shared = dir.path().join("checkpoints");
    std::fs::create_dir_all(shared.join("epoch-1")).unwrap();
    std::fs::write(shared.join("config.json"), b"{}").unwrap();
    let big: Vec<u8> = (0..900_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(shared.join("epoch-1/model.pt"), &big).unwrap();
    assert!(matches!(
        local(&h, Request::SetFiles { on: true }).await,
        Response::Files(_)
    ));
    let path = shared.to_str().unwrap().to_string();
    assert!(matches!(
        local(&h, Request::ShareFolder { path }).await,
        Response::Files(_)
    ));
    let phone = h.phone(Some(Permission::Control)).await;
    let m = h.connect(&phone).await.unwrap();

    let dest = dir.path().join("here");
    std::fs::create_dir_all(&dest).unwrap();
    let mut known = Known::default();
    assert_eq!(
        round(&m, &dest, &mut known).await,
        ["config.json", "epoch-1/model.pt"]
    );
    assert_eq!(std::fs::read(dest.join("epoch-1/model.pt")).unwrap(), big);
    assert!(round(&m, &dest, &mut known).await.is_empty());

    // A new file and a changed one come; a file changed here is kept.
    std::fs::write(shared.join("epoch-1/opt.pt"), b"optimizer").unwrap();
    std::fs::write(dest.join("config.json"), b"{\"mine\": 1}").unwrap();
    std::fs::write(shared.join("config.json"), b"{\"lr\": 3}").unwrap();
    let t = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(shared.join("config.json"))
        .unwrap()
        .set_modified(t)
        .unwrap();
    assert_eq!(
        round(&m, &dest, &mut known).await,
        ["config.json", "epoch-1/opt.pt"]
    );
    assert_eq!(
        std::fs::read(dest.join("config.json")).unwrap(),
        b"{\"mine\": 1}"
    );
    assert_eq!(
        std::fs::read(dest.join("config (2).json")).unwrap(),
        b"{\"lr\": 3}"
    );

    // A download cut off half way carries on from the partial copy.
    let big2: Vec<u8> = (0..800_000u32).map(|i| (i % 241) as u8).collect();
    std::fs::write(shared.join("epoch-2.pt"), &big2).unwrap();
    let partial = dest.join(PARTIAL).join("epoch-2.pt");
    std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
    std::fs::write(&partial, &big2[..300_000]).unwrap();
    let mut first = None;
    let remote = m.sync_scan(Root::Shared(0)).await.unwrap();
    let f = needed(&remote, &known)
        .into_iter()
        .find(|f| f.path == "epoch-2.pt")
        .unwrap();
    m.sync_get(Root::Shared(0), &f, &dest, None, |done, _| {
        first.get_or_insert(done);
    })
    .await
    .unwrap();
    assert_eq!(first, Some(300_000));
    assert_eq!(std::fs::read(dest.join("epoch-2.pt")).unwrap(), big2);
    assert!(!partial.exists());
}
