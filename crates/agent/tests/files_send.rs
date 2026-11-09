//! F40 over a real connection: sending files into the Inbox (never over another file,
//! resumed, refused when too big) and files offered with `pgpu send`.

mod harness;

use std::path::PathBuf;

use harness::{Harness, Phone};
use pocketgpu_client::{ClientError, Machine};
use pocketgpu_proto::files::{self, Root, Upload};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::{ErrorCode, Permission, alpn, read_frame, write_frame};

fn code<T: std::fmt::Debug>(r: Result<T, ClientError>) -> ErrorCode {
    match r {
        Err(ClientError::Remote(e)) => e.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

struct Setup {
    h: Harness,
    dir: tempfile::TempDir,
    inbox: PathBuf,
}

async fn setup() -> Setup {
    let h = Harness::start().await;
    // Not in the system temp folder: on Windows that is in AppData, which can't be shared.
    let dir = tempfile::Builder::new()
        .prefix("pgpu-send-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let inbox = dir.path().join("Inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    {
        let mut cfg = h.agent.config.write().unwrap();
        cfg.files = true;
        cfg.files_inbox = Some(inbox.clone());
    }
    Setup { h, dir, inbox }
}

async fn phone(h: &Harness, p: Permission) -> (Phone, Machine) {
    let phone = h.phone(Some(p)).await;
    let m = h.connect(&phone).await.unwrap();
    (phone, m)
}

impl Setup {
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Starts a send by hand: the `Put`, `Ready`, then these bytes, then the stream ends.
    async fn put_raw(
        &self,
        phone: &Phone,
        size: u64,
        transfer: [u8; 16],
        bytes: &[u8],
    ) -> Vec<files::Response> {
        let conn = phone
            .client
            .dial(self.h.router.endpoint().addr(), alpn::FILES)
            .await
            .unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let req = files::Request::Put {
            name: "big.bin".into(),
            size,
            transfer,
        };
        write_frame(&mut send, &req).await.unwrap();
        let mut replies = Vec::new();
        let first = read_frame::<_, files::Response>(&mut recv)
            .await
            .unwrap()
            .unwrap();
        let ready = matches!(first, files::Response::Ready { .. });
        replies.push(first);
        if ready {
            if !bytes.is_empty() {
                let _ = write_frame(&mut send, &Upload::Chunk(bytes.to_vec())).await;
            }
            let _ = send.finish();
            // The agent is done with the stream when it ends its side.
            while let Ok(Some(r)) = read_frame::<_, files::Response>(&mut recv).await {
                replies.push(r);
            }
        }
        replies
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sent_files_land_in_the_inbox_under_a_free_name() {
    let s = setup().await;
    let (_p, control) = phone(&s.h, Permission::Control).await;
    let src = s.file("photo.jpg", b"first picture");
    let saved = control
        .upload(&src, "photo.jpg", [1; 16], |_, _| {})
        .await
        .unwrap();
    assert_eq!(saved.name, "photo.jpg");
    assert_eq!(saved.blake3, *blake3::hash(b"first picture").as_bytes());

    let src2 = s.file("other.jpg", b"second picture");
    let saved = control
        .upload(&src2, "photo.jpg", [2; 16], |_, _| {})
        .await
        .unwrap();
    assert_eq!(saved.name, "photo (2).jpg");
    assert_eq!(
        std::fs::read(s.inbox.join("photo.jpg")).unwrap(),
        b"first picture"
    );
    assert_eq!(
        std::fs::read(s.inbox.join("photo (2).jpg")).unwrap(),
        b"second picture"
    );

    // Folder parts and odd characters don't survive; nothing partial is left.
    let saved = control
        .upload(&src, "../../x:y.txt", [3; 16], |_, _| {})
        .await
        .unwrap();
    assert_eq!(saved.name, "x_y.txt");
    let left = |end: &str| {
        std::fs::read_dir(s.inbox.join(".partial"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(end))
            .count()
    };
    assert_eq!(left(".part"), 0);
    // A note of each name stays a day, for a device that missed the reply.
    assert_eq!(left(".done"), 3);

    // The Inbox lists them with the device that sent them; Control devices can open it.
    let list = match pocketgpu::files::local_request(&s.h.agent, Request::Inbox).await {
        Response::InboxList(l) => l,
        other => panic!("{other:?}"),
    };
    assert_eq!(list.len(), 3);
    assert!(list.iter().all(|f| f.from == "Test phone"), "{list:?}");
    let (entries, _) = control.files_list(Root::Inbox, "", 0).await.unwrap();
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert_eq!(s.h.audit_events("file_put"), 3);
    assert_eq!(control.files_overview().await.unwrap().inbox_files, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_that_stopped_carries_on() {
    let s = setup().await;
    let (p, control) = phone(&s.h, Permission::Control).await;
    let data: Vec<u8> = (0..600_000u32).map(|i| (i % 253) as u8).collect();
    let src = s.file("big.bin", &data);
    let transfer = [7; 16];
    let replies = s
        .put_raw(&p, data.len() as u64, transfer, &data[..200_000])
        .await;
    assert_eq!(replies, vec![files::Response::Ready { have: 0 }]);

    let mut first = None;
    let saved = control
        .upload(&src, "big.bin", transfer, |have, _| {
            first.get_or_insert(have);
        })
        .await
        .unwrap();
    assert_eq!(first, Some(200_000));
    assert_eq!(saved.name, "big.bin");
    assert_eq!(std::fs::read(s.inbox.join("big.bin")).unwrap(), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_changed_between_tries_is_not_saved() {
    let s = setup().await;
    let (p, control) = phone(&s.h, Permission::Control).await;
    let old: Vec<u8> = (0..600_000u32).map(|i| (i % 253) as u8).collect();
    let src = s.file("big.bin", &old);
    let transfer = [8; 16];
    s.put_raw(&p, old.len() as u64, transfer, &old[..200_000])
        .await;
    // Rewritten since, with the same size: the start sent before is not this file's.
    let new: Vec<u8> = old.iter().map(|b| b ^ 0xff).collect();
    std::fs::write(&src, &new).unwrap();
    let up = control.upload(&src, "big.bin", transfer, |_, _| {}).await;
    match up {
        Err(ClientError::Remote(e)) => {
            assert_eq!(e.code, ErrorCode::Invalid);
            assert!(e.message.contains("changed"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
    assert!(!s.inbox.join("big.bin").exists());
    // Trying again starts over and saves the file as it is now, under its own name.
    let mut first = None;
    let saved = control
        .upload(&src, "big.bin", transfer, |have, _| {
            first.get_or_insert(have);
        })
        .await
        .unwrap();
    assert_eq!((first, saved.name.as_str()), (Some(0), "big.bin"));
    assert_eq!(std::fs::read(s.inbox.join("big.bin")).unwrap(), new);
    let differs = s.h.audit_events("file_put");
    assert_eq!(differs, 2, "the refused one is audited too");
}

#[tokio::test(flavor = "multi_thread")]
async fn trying_a_saved_send_again_does_not_save_it_twice() {
    let s = setup().await;
    let (_p, control) = phone(&s.h, Permission::Control).await;
    let src = s.file("data.csv", b"a,b\n1,2\n");
    let saved = control
        .upload(&src, "data.csv", [9; 16], |_, _| {})
        .await
        .unwrap();
    // The reply was lost; the app tries again with the same transfer.
    let mut seen = Vec::new();
    let again = control
        .upload(&src, "data.csv", [9; 16], |have, size| {
            seen.push((have, size))
        })
        .await
        .unwrap();
    assert_eq!(again, saved);
    assert_eq!(seen, vec![(8, 8)], "nothing is sent again");
    assert!(!s.inbox.join("data (2).csv").exists());
    assert_eq!(s.h.audit_events("file_put"), 1);
    // Another file under the same transfer is refused, and the next try starts over.
    std::fs::write(&src, b"x,y\n3,4\n").unwrap();
    let other = control.upload(&src, "data.csv", [9; 16], |_, _| {}).await;
    assert_eq!(code(other), ErrorCode::Invalid);
    let next = control.upload(&src, "data.csv", [9; 16], |_, _| {}).await;
    assert_eq!(next.unwrap().name, "data (2).csv");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_of_another_size_starts_over() {
    let s = setup().await;
    let (p, control) = phone(&s.h, Permission::Control).await;
    let data = vec![7u8; 300_000];
    s.put_raw(&p, 400_000, [4; 16], &data[..100_000]).await;
    let src = s.file("a.bin", &data);
    let mut first = None;
    let saved = control
        .upload(&src, "a.bin", [4; 16], |have, _| {
            first.get_or_insert(have);
        })
        .await
        .unwrap();
    assert_eq!(first, Some(0));
    assert_eq!(std::fs::read(s.inbox.join(saved.name)).unwrap(), data);
    let partials = std::fs::read_dir(s.inbox.join(".partial"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .count();
    assert_eq!(partials, 0, "the other size's partial is gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn too_big_or_too_long_is_refused() {
    let s = setup().await;
    let (p, _control) = phone(&s.h, Permission::Control).await;
    // More than the disk has: refused before it starts.
    let replies = s.put_raw(&p, u64::MAX / 4, [1; 16], &[]).await;
    match &replies[..] {
        [files::Response::Error(e)] => {
            assert_eq!(e.code, ErrorCode::Invalid);
            assert!(e.message.starts_with("not enough space"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
    // More bytes than it said: refused, nothing saved.
    let replies = s.put_raw(&p, 3, [2; 16], b"0123456789").await;
    match &replies[..] {
        [
            files::Response::Ready { have: 0 },
            files::Response::Error(e),
        ] => {
            assert_eq!(e.code, ErrorCode::Invalid);
        }
        other => panic!("{other:?}"),
    }
    assert!(!s.inbox.join("big.bin").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn offered_files_can_be_saved_for_a_week() {
    let s = setup().await;
    let (_v, view) = phone(&s.h, Permission::View).await;
    let (_c, control) = phone(&s.h, Permission::Control).await;
    let src = s.file("adapter.safetensors", b"weights");
    let path = src.to_str().unwrap().to_string();
    let offered = pocketgpu::files::local_request(&s.h.agent, Request::Offer { path, to: None });
    match offered.await {
        Response::Offered { name, size, to, .. } => {
            assert_eq!((name.as_str(), size), ("adapter.safetensors", 7));
            assert_eq!(to, vec!["Test phone".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    assert!(view.files_overview().await.unwrap().offers.is_empty());
    let offers = control.files_overview().await.unwrap().offers;
    assert_eq!(offers.len(), 1);
    let id = offers[0].id.clone();
    let dest = s.dir.path().join("saved");
    let got = control
        .download(Root::Offers, &id, &dest, |_, _| {})
        .await
        .unwrap();
    assert_eq!(got.size, 7);
    assert_eq!(std::fs::read(&dest).unwrap(), b"weights");
    let dest_v = s.dir.path().join("v");
    let get = view.download(Root::Offers, &id, &dest_v, |_, _| {});
    assert_eq!(code(get.await), ErrorCode::Forbidden);

    // Changed since: refused.
    std::fs::write(&src, b"new weights").unwrap();
    let dest_c = s.dir.path().join("c");
    let get = control.download(Root::Offers, &id, &dest_c, |_, _| {});
    assert_eq!(code(get.await), ErrorCode::NotFound);

    // Expired: gone.
    std::fs::write(&src, b"weights").unwrap();
    s.h.agent
        .db()
        .execute("UPDATE offers SET expires_ms = 0", [])
        .unwrap();
    assert!(control.files_overview().await.unwrap().offers.is_empty());
    let dest_e = s.dir.path().join("e");
    let get = control.download(Root::Offers, &id, &dest_e, |_, _| {});
    assert_eq!(code(get.await), ErrorCode::NotFound);
    assert_eq!(s.h.audit_events("file_offer"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_offer_to_one_device_reaches_only_it() {
    let s = setup().await;
    let (pixel, to_pixel) = phone(&s.h, Permission::Control).await;
    let (_o, other) = phone(&s.h, Permission::Control).await;
    let (viewer, _) = phone(&s.h, Permission::View).await;
    {
        let db = s.h.agent.db();
        let rename = "UPDATE devices SET name = ?1 WHERE device_id = ?2";
        db.execute(rename, ["Pixel 8", &pixel.key.public().to_string()])
            .unwrap();
        db.execute(rename, ["Old tablet", &viewer.key.public().to_string()])
            .unwrap();
    }
    let src = s.file("out.png", b"png");
    let path = src.to_str().unwrap().to_string();
    let offer = |to: &str| {
        let req = Request::Offer {
            path: path.clone(),
            to: Some(to.into()),
        };
        pocketgpu::files::local_request(&s.h.agent, req)
    };
    match offer("pixel 8").await {
        Response::Offered { to, .. } => assert_eq!(to, vec!["Pixel 8".to_string()]),
        other => panic!("{other:?}"),
    }
    assert_eq!(to_pixel.files_overview().await.unwrap().offers.len(), 1);
    assert!(other.files_overview().await.unwrap().offers.is_empty());
    match offer("Old tablet").await {
        Response::Error(e) => assert!(e.contains("can only view"), "{e}"),
        other => panic!("{other:?}"),
    }
}
