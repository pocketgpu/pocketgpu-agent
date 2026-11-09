//! F40 over a real connection: every files request is checked against the device's
//! permission before anything else; browsing and saving files from the machine.

mod harness;

use std::path::{Path, PathBuf};

use harness::{Harness, Phone};
use pocketgpu_client::{ClientError, Machine};
use pocketgpu_proto::files::{Kind, Root};
use pocketgpu_proto::local::{Request, Response};
use pocketgpu_proto::{ErrorCode, Permission};

fn code<T: std::fmt::Debug>(r: Result<T, ClientError>) -> ErrorCode {
    match r {
        Err(ClientError::Remote(e)) => e.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// An agent with files on, `output` shared (two pictures, a folder, a hidden file) and
/// the Inbox in a temporary folder.
struct Setup {
    h: Harness,
    dir: tempfile::TempDir,
    shared: PathBuf,
}

async fn local(h: &Harness, req: Request) -> Response {
    pocketgpu::files::local_request(&h.agent, req).await
}

async fn setup() -> Setup {
    let h = Harness::start().await;
    // Not in the system temp folder: on Windows that is in AppData, which can't be shared.
    let dir = tempfile::Builder::new()
        .prefix("pgpu-files-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let shared = dir.path().join("output");
    std::fs::create_dir_all(shared.join("runs")).unwrap();
    std::fs::write(shared.join("old.png"), b"old").unwrap();
    std::fs::write(shared.join(".env"), b"TOKEN=1").unwrap();
    let data: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(shared.join("new.png"), &data).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(shared.join("old.png"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::create_dir_all(dir.path().join("Inbox")).unwrap();
    h.agent.config.write().unwrap().files_inbox = Some(dir.path().join("Inbox"));
    assert!(
        matches!(local(&h, Request::SetFiles { on: true }).await, Response::Files(s) if s.enabled)
    );
    let path = shared.to_str().unwrap().to_string();
    match local(&h, Request::ShareFolder { path }).await {
        Response::Files(s) => assert_eq!(s.shared.len(), 1),
        other => panic!("{other:?}"),
    }
    Setup { h, dir, shared }
}

async fn phone(h: &Harness, p: Permission) -> (Phone, Machine) {
    let phone = h.phone(Some(p)).await;
    let m = h.connect(&phone).await.unwrap();
    (phone, m)
}

fn hash(path: &Path) -> [u8; 32] {
    *blake3::hash(&std::fs::read(path).unwrap()).as_bytes()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_root_needs_its_permission() {
    let s = setup().await;
    let (_v, view) = phone(&s.h, Permission::View).await;
    let (_c, control) = phone(&s.h, Permission::Control).await;
    for root in [Root::Shared(0), Root::Inbox, Root::Offers] {
        assert_eq!(
            code(view.files_list(root, "", 0).await),
            ErrorCode::Forbidden
        );
    }
    assert_eq!(
        code(control.files_list(Root::Home, "", 0).await),
        ErrorCode::Forbidden
    );
    let src = s.dir.path().join("a.txt");
    std::fs::write(&src, b"hello").unwrap();
    let put = view.upload(&src, "a.txt", [1; 16], |_, _| {}).await;
    assert_eq!(code(put), ErrorCode::Forbidden);
    let get = control
        .download(Root::Home, "x", &s.dir.path().join("x"), |_, _| {})
        .await;
    assert_eq!(code(get), ErrorCode::Forbidden);
    assert!(s.h.audit_events("refused") >= 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn off_is_refused_but_says_so() {
    let s = setup().await;
    local(&s.h, Request::SetFiles { on: false }).await;
    let (_c, control) = phone(&s.h, Permission::Control).await;
    let o = control.files_overview().await.unwrap();
    assert!(!o.enabled && o.shared.is_empty() && o.inbox.is_empty());
    let list = control.files_list(Root::Shared(0), "", 0).await;
    match list {
        Err(ClientError::Remote(e)) => {
            assert_eq!(e.code, ErrorCode::Forbidden);
            assert!(e.message.contains("pgpu files on"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
    let dest = s.dir.path().join("x.png");
    let get = control.download(Root::Shared(0), "new.png", &dest, |_, _| {});
    assert_eq!(code(get.await), ErrorCode::Forbidden);
}

#[tokio::test(flavor = "multi_thread")]
async fn view_reads_job_logs_only() {
    let s = setup().await;
    let jobs = s.h.agent.paths.jobs_dir();
    std::fs::write(jobs.join("7.log"), b"epoch 1 loss 0.4\n").unwrap();
    std::fs::write(jobs.join("notes.txt"), b"not a log").unwrap();
    let (_v, view) = phone(&s.h, Permission::View).await;
    let o = view.files_overview().await.unwrap();
    assert!(o.enabled && o.job_logs == 1 && o.offers.is_empty() && !o.home);
    assert_eq!(o.shared[0].name, "output");
    let (entries, _) = view.files_list(Root::JobLogs, "", 0).await.unwrap();
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["7.log"]);
    let dest = s.dir.path().join("7.log");
    let got = view.download(Root::JobLogs, "7.log", &dest, |_, _| {});
    assert_eq!(got.await.unwrap().size, 17);
    let dest_n = s.dir.path().join("n");
    let other = view.download(Root::JobLogs, "notes.txt", &dest_n, |_, _| {});
    assert_eq!(code(other.await), ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn control_lists_shared_folders_newest_first() {
    let s = setup().await;
    let (_c, control) = phone(&s.h, Permission::Control).await;
    let o = control.files_overview().await.unwrap();
    assert_eq!(o.shared.len(), 1);
    assert_eq!(o.shared[0].files, 2);
    assert_eq!(o.relay_limit, Some(pocketgpu_proto::files::RELAY_LIMIT));
    let (entries, next) = control.files_list(Root::Shared(0), "", 0).await.unwrap();
    let names: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(
        names,
        [
            ("runs", Kind::Folder),
            ("new.png", Kind::File),
            ("old.png", Kind::File)
        ]
    );
    assert_eq!(next, None);
    assert!(
        control
            .files_list(Root::Shared(0), "runs", 0)
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(
        code(control.files_list(Root::Shared(3), "", 0).await),
        ErrorCode::NotFound
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_whole_and_resumed_with_the_hash_checked() {
    let s = setup().await;
    let (_c, control) = phone(&s.h, Permission::Control).await;
    let src = s.shared.join("new.png");
    let dest = s.dir.path().join("whole.png");
    let mut seen = Vec::new();
    let got = control
        .download(Root::Shared(0), "new.png", &dest, |done, size| {
            seen.push((done, size))
        })
        .await
        .unwrap();
    assert_eq!(got.size, 700_000);
    assert_eq!(got.blake3, hash(&src));
    assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&src).unwrap());
    assert_eq!(seen.first(), Some(&(0, 700_000)));
    assert_eq!(seen.last(), Some(&(700_000, 700_000)));

    // The connection dropped at 300 000 bytes: saving again carries on from there.
    let part = s.dir.path().join("part.png");
    std::fs::write(&part, &std::fs::read(&src).unwrap()[..300_000]).unwrap();
    let mut first = None;
    let got = control
        .download(Root::Shared(0), "new.png", &part, |done, _| {
            first.get_or_insert(done);
        })
        .await
        .unwrap();
    assert_eq!(first, Some(300_000));
    assert_eq!(got.blake3, hash(&src));
    assert_eq!(std::fs::read(&part).unwrap(), std::fs::read(&src).unwrap());

    // A start that isn't the file's: the whole file's hash catches it.
    let bad = s.dir.path().join("bad.png");
    std::fs::write(&bad, vec![0u8; 1000]).unwrap();
    let got = control.download(Root::Shared(0), "new.png", &bad, |_, _| {});
    assert!(matches!(got.await, Err(ClientError::HashMismatch)));
    assert_eq!(s.h.audit_events("file_get"), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn paths_cannot_leave_the_folder() {
    let s = setup().await;
    std::fs::write(s.dir.path().join("secret.txt"), b"no").unwrap();
    let (_c, control) = phone(&s.h, Permission::Control).await;
    let get = |path: &'static str| {
        let dest = s.dir.path().join("dest");
        let _ = std::fs::remove_file(&dest);
        let control = &control;
        async move {
            code(
                control
                    .download(Root::Shared(0), path, &dest, |_, _| {})
                    .await,
            )
        }
    };
    assert_eq!(get("../secret.txt").await, ErrorCode::Invalid);
    assert_eq!(get("runs/../../secret.txt").await, ErrorCode::Invalid);
    assert_eq!(get("/etc/passwd").await, ErrorCode::Invalid);
    assert_eq!(get(".env").await, ErrorCode::NotFound);
    assert_eq!(get("runs").await, ErrorCode::Invalid);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(s.dir.path().join("secret.txt"), s.shared.join("link")).unwrap();
        assert_eq!(get("link").await, ErrorCode::Forbidden);
        std::os::unix::fs::symlink(s.dir.path(), s.shared.join("up")).unwrap();
        let listed = control.files_list(Root::Shared(0), "up", 0).await;
        assert_eq!(code(listed), ErrorCode::Forbidden);
        let (entries, _) = control.files_list(Root::Shared(0), "", 0).await.unwrap();
        assert!(entries.iter().all(|e| e.name != "link" && e.name != "up"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn all_your_files_needs_shell_and_the_terminal() {
    let s = setup().await;
    let (_p, shell) = phone(&s.h, Permission::Shell).await;
    assert!(!shell.files_overview().await.unwrap().home);
    let refused = shell.files_list(Root::Home, "", 0).await;
    match refused {
        Err(ClientError::Remote(e)) => {
            assert_eq!(e.code, ErrorCode::Forbidden);
            assert!(e.message.contains("pgpu shell on"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
    s.h.agent.config.write().unwrap().shell = true;
    // As root in a container the terminal stays off, and so does this.
    let root_in_container = matches!(
        pocketgpu::daemon::root::check(),
        Ok(pocketgpu::daemon::root::RootCheck::RootInContainer)
    );
    let home = shell.files_overview().await.unwrap().home;
    assert_eq!(home, !root_in_container);
    if root_in_container {
        let refused = shell.files_list(Root::Home, "", 0).await;
        assert_eq!(code(refused), ErrorCode::Forbidden);
    } else if pocketgpu::files::home().is_some() {
        shell.files_list(Root::Home, "", 0).await.unwrap();
    }
    let (_c, control) = phone(&s.h, Permission::Control).await;
    assert!(!control.files_overview().await.unwrap().home);
    assert_eq!(
        code(control.files_list(Root::Home, "", 0).await),
        ErrorCode::Forbidden
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn risky_folders_are_not_shared() {
    let s = setup().await;
    let refused = |r: Response| match r {
        Response::Error(e) => e,
        other => panic!("{other:?}"),
    };
    let e = refused(
        local(
            &s.h,
            Request::ShareFolder {
                path: "/etc".into(),
            },
        )
        .await,
    );
    assert!(e.contains("system folder"), "{e}");
    let own = s.h.agent.paths.data_dir.to_str().unwrap().to_string();
    let e = refused(local(&s.h, Request::ShareFolder { path: own }).await);
    assert!(e.contains("PocketGPU's own"), "{e}");
    if let Some(home) = pocketgpu::files::home().filter(|h| h.parent().is_some()) {
        let path = home.to_str().unwrap().to_string();
        let e = refused(local(&s.h, Request::ShareFolder { path }).await);
        assert!(e.contains("home folder"), "{e}");
    }
    // Unsharing by the path as shown, then the folder is gone for devices.
    let shown = match local(&s.h, Request::GetFiles).await {
        Response::Files(f) => f.shared[0].clone(),
        other => panic!("{other:?}"),
    };
    match local(&s.h, Request::UnshareFolder { path: shown }).await {
        Response::Files(f) => assert!(f.shared.is_empty()),
        other => panic!("{other:?}"),
    }
    let (_c, control) = phone(&s.h, Permission::Control).await;
    assert_eq!(
        code(control.files_list(Root::Shared(0), "", 0).await),
        ErrorCode::NotFound
    );
    assert!(s.h.audit_events("files_access") >= 3);
}
