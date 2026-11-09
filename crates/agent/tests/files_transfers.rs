//! F40 transfers over a real connection: at most 4 per device with their slots back when the
//! device leaves, every get in the audit log however it ends, transfers that stop when what
//! they need goes away, and an Inbox that leaves no unfinished sends behind when it moves.

mod harness;

use std::path::PathBuf;
use std::time::Duration;

use harness::{Harness, Phone};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use pocketgpu::store::devices;
use pocketgpu_proto::files::{self, Root, Upload};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::{ErrorCode, Permission, alpn, read_frame, write_frame};

/// Bigger than what a stream holds unread, so a get waits for the device to read on.
const BIG: usize = 16 << 20;

struct Setup {
    h: Harness,
    dir: tempfile::TempDir,
}

/// Files on, `out` shared with a 16 MB `big.bin`, and the Inbox in a temporary folder.
async fn setup() -> Setup {
    let h = Harness::start().await;
    // Not in the system temp folder: on Windows that is in AppData, which can't be shared.
    let dir = tempfile::Builder::new()
        .prefix("pgpu-transfers-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let shared = dir.path().join("out");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("big.bin"), vec![5u8; BIG]).unwrap();
    std::fs::create_dir_all(dir.path().join("Inbox")).unwrap();
    {
        let mut cfg = h.agent.config.write().unwrap();
        cfg.files = true;
        cfg.files_inbox = Some(dir.path().join("Inbox"));
    }
    let path = shared.to_str().unwrap().to_string();
    let shared = pocketgpu::files::local_request(&h.agent, Request::ShareFolder { path }).await;
    assert!(matches!(shared, Response::Files(_)), "{shared:?}");
    Setup { h, dir }
}

async fn dial(s: &Setup, phone: &Phone) -> Connection {
    phone
        .client
        .dial(s.h.router.endpoint().addr(), alpn::FILES)
        .await
        .unwrap()
}

/// Starts a get of `big.bin` and reads the first reply.
async fn start_get(conn: &Connection) -> (SendStream, RecvStream, files::Response) {
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    let req = files::Request::Get {
        root: Root::Shared(0),
        path: "big.bin".into(),
        from: 0,
    };
    write_frame(&mut send, &req).await.unwrap();
    send.finish().unwrap();
    let first = read_frame(&mut recv).await.unwrap().unwrap();
    (send, recv, first)
}

/// Reads on until the get ends: the error it ended with, or `None` after `Done`.
async fn read_to_end(recv: &mut RecvStream) -> Option<pocketgpu_proto::Error> {
    loop {
        match read_frame::<_, files::Response>(recv).await.unwrap() {
            Some(files::Response::Chunk(_)) => continue,
            Some(files::Response::Done { .. }) => return None,
            Some(files::Response::Error(e)) => return Some(e),
            other => panic!("{other:?}"),
        }
    }
}

/// Results of the `file_get` entries, oldest first.
fn gets(h: &Harness) -> Vec<String> {
    let mut entries = pocketgpu::audit::since(&h.agent.db(), 0).unwrap();
    entries.reverse();
    entries
        .into_iter()
        .filter(|e| e.event == "file_get")
        .map(|e| e.result)
        .collect()
}

/// Waits up to 5 s for `done`.
async fn eventually(mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..250 {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn four_transfers_per_device_and_their_slots_come_back() {
    let s = setup().await;
    let p = s.h.phone(Some(Permission::Control)).await;
    let conn = dial(&s, &p).await;
    let mut held = Vec::new();
    for _ in 0..4 {
        let (send, recv, head) = start_get(&conn).await;
        assert!(matches!(head, files::Response::Head { .. }), "{head:?}");
        held.push((send, recv));
    }
    match start_get(&conn).await.2 {
        files::Response::Error(e) => {
            assert_eq!(e.code, ErrorCode::Busy);
            assert!(e.message.contains("4 files"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
    // Another device has its own four.
    let other = s.h.phone(Some(Permission::Control)).await;
    let other_conn = dial(&s, &other).await;
    let head = start_get(&other_conn).await.2;
    assert!(matches!(head, files::Response::Head { .. }), "{head:?}");

    // The device leaves halfway: its slots come back.
    conn.close(0u32.into(), b"bye");
    drop(held);
    let conn = dial(&s, &p).await;
    let mut started = false;
    for _ in 0..250 {
        if let (_, _, files::Response::Head { .. }) = start_get(&conn).await {
            started = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(started, "the slots never came back");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_get_is_audited_however_it_ends() {
    let s = setup().await;
    let p = s.h.phone(Some(Permission::Control)).await;
    let conn = dial(&s, &p).await;
    let (_send, mut recv, _head) = start_get(&conn).await;
    let chunk = read_frame::<_, files::Response>(&mut recv).await.unwrap();
    assert!(matches!(chunk, Some(files::Response::Chunk(_))));
    // In the log (and so in a notice) as the bytes start to go.
    assert_eq!(gets(&s.h), ["started"]);
    let target = &pocketgpu::audit::since(&s.h.agent.db(), 0).unwrap()[0].target;
    assert_eq!(target, "out/big.bin");

    // The device takes most of it and leaves: the entry says where it stopped.
    conn.close(0u32.into(), b"bye");
    assert!(eventually(|| gets(&s.h)[0] != "started").await);
    let result = &gets(&s.h)[0];
    assert!(
        result.starts_with("stopped at ") && result.ends_with(" of 16 MB"),
        "{result}"
    );

    // A whole one ends as ok.
    let conn = dial(&s, &p).await;
    let (_send, mut recv, _) = start_get(&conn).await;
    assert_eq!(read_to_end(&mut recv).await, None);
    assert!(eventually(|| gets(&s.h).get(1).is_some_and(|r| r == "ok")).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lowered_permission_or_an_unshared_folder_stops_a_get() {
    let s = setup().await;
    let p = s.h.phone(Some(Permission::Control)).await;
    let key = p.key.public().to_string();
    let conn = dial(&s, &p).await;
    let (_send, mut recv, _) = start_get(&conn).await;
    devices::set_permission(&s.h.agent.db(), &key, Permission::View).unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let e = read_to_end(&mut recv)
        .await
        .expect("stopped before the end");
    assert_eq!(e.code, ErrorCode::Forbidden);
    assert!(eventually(|| gets(&s.h)[0].contains("needs control permission")).await);

    devices::set_permission(&s.h.agent.db(), &key, Permission::Control).unwrap();
    let (_send, mut recv, _) = start_get(&conn).await;
    let shown = match pocketgpu::files::local_request(&s.h.agent, Request::GetFiles).await {
        Response::Files(f) => f.shared[0].clone(),
        other => panic!("{other:?}"),
    };
    pocketgpu::files::local_request(&s.h.agent, Request::UnshareFolder { path: shown }).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let e = read_to_end(&mut recv)
        .await
        .expect("stopped before the end");
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.message.contains("no longer shared"), "{}", e.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moved_inbox_leaves_no_unfinished_sends() {
    let s = setup().await;
    let p = s.h.phone(Some(Permission::Control)).await;
    let conn = dial(&s, &p).await;
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    let req = files::Request::Put {
        name: "half.bin".into(),
        size: 1000,
        transfer: [3; 16],
    };
    write_frame(&mut send, &req).await.unwrap();
    let ready = read_frame::<_, files::Response>(&mut recv).await.unwrap();
    assert_eq!(ready, Some(files::Response::Ready { have: 0 }));
    write_frame(&mut send, &Upload::Chunk(vec![1; 400]))
        .await
        .unwrap();
    send.finish().unwrap();
    while let Ok(Some(_)) = read_frame::<_, files::Response>(&mut recv).await {}
    let old: PathBuf = s.dir.path().join("Inbox").join(".partial");
    assert_eq!(std::fs::read_dir(&old).unwrap().count(), 1);

    // The same folder again, known by another path: nothing moved, nothing goes.
    #[cfg(unix)]
    {
        let link = s.dir.path().join("InboxLink");
        std::os::unix::fs::symlink(s.dir.path().join("Inbox"), &link).unwrap();
        s.h.agent.config.write().unwrap().files_inbox = Some(link);
        let path = s.dir.path().join("Inbox").to_str().unwrap().to_string();
        let same = pocketgpu::files::local_request(&s.h.agent, Request::SetInbox { path }).await;
        assert!(matches!(same, Response::Files(_)), "{same:?}");
        assert_eq!(std::fs::read_dir(&old).unwrap().count(), 1);
    }

    let new = s.dir.path().join("NewInbox");
    std::fs::create_dir_all(&new).unwrap();
    let path = new.to_str().unwrap().to_string();
    let moved = pocketgpu::files::local_request(&s.h.agent, Request::SetInbox { path }).await;
    assert!(matches!(moved, Response::Files(_)), "{moved:?}");
    assert!(!old.exists(), "the old Inbox's unfinished sends stay");
}
