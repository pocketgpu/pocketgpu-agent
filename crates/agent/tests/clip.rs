//! F41 over a real connection, with PocketGPU's own clipboard in memory: off until turned
//! on at the machine, putting text needs Control and reading it Shell, checked before
//! anything else, and the text never reaches the audit log.

mod harness;

use std::sync::Once;

use harness::Harness;
use pocketgpu::clip::local_request;
use pocketgpu_client::{ClientError, ClipContent, Machine};
use pocketgpu_proto::clip::{MAX_TEXT, Place};
use pocketgpu_proto::local::{Request as Local, Response as Reply};
use pocketgpu_proto::{ErrorCode, Permission};

/// Every test starts here, so the agent never touches this computer's own clipboard.
fn memory_clipboard() {
    static SET: Once = Once::new();
    // SAFETY: every test calls this before it starts anything, and this binary sets no
    // other environment.
    SET.call_once(|| unsafe { std::env::set_var("PGPU_CLIPBOARD", "memory") });
}

async fn start(on: bool) -> Harness {
    memory_clipboard();
    let h = Harness::start().await;
    if on {
        let reply = local_request(&h.agent, Local::SetClip { on: true }).await;
        assert!(matches!(&reply, Reply::Clip(s) if s.enabled), "{reply:?}");
    }
    h
}

async fn device(h: &Harness, permission: Permission) -> Machine {
    h.connect(&h.phone(Some(permission)).await).await.unwrap()
}

fn refusal<T: std::fmt::Debug>(r: Result<T, ClientError>) -> (ErrorCode, String) {
    match r {
        Err(ClientError::Remote(e)) => (e.code, e.message),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn text(content: ClipContent) -> String {
    match content {
        ClipContent::Text { text, .. } => text,
        other => panic!("expected text, got {other:?}"),
    }
}

/// Audit entries of one kind, as (target, result).
fn audited(h: &Harness, event: &str) -> Vec<(String, String)> {
    pocketgpu::audit::since(&h.agent.db(), 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.event == event)
        .map(|e| (e.target, e.result))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn off_until_turned_on_at_the_machine() {
    let h = start(false).await;
    let view = device(&h, Permission::View).await;
    let control = device(&h, Permission::Control).await;
    let shell = device(&h, Permission::Shell).await;

    // Info still answers, so a phone can say how to turn it on.
    let info = view.clip_info().await.unwrap();
    assert!(!info.enabled);
    assert_eq!(info.place, Place::Memory);
    assert_eq!(info.clears_after_s, Some(600));
    let (code, message) = refusal(control.clip_put("hf_abc", false, false).await);
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(message.contains("pgpu clip on"), "{message}");
    assert_eq!(refusal(shell.clip_get().await).0, ErrorCode::Forbidden);
    assert!(audited(&h, "clip_put").is_empty());

    let reply = local_request(&h.agent, Local::SetClip { on: true }).await;
    assert!(matches!(&reply, Reply::Clip(s) if s.enabled), "{reply:?}");
    assert!(h.agent.config.read().unwrap().clipboard);
    assert_eq!(
        audited(&h, "clip_access"),
        [("clipboard".into(), "on".into())]
    );
    assert!(view.clip_info().await.unwrap().enabled);
    control.clip_put("hf_abc", false, false).await.unwrap();

    // Off forgets what memory holds.
    let reply = local_request(&h.agent, Local::SetClip { on: false }).await;
    assert!(
        matches!(&reply, Reply::Clip(s) if !s.enabled && !s.has_text),
        "{reply:?}"
    );
    assert!(matches!(
        local_request(&h.agent, Local::ClipGet).await,
        Reply::ClipText(None)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn view_only_sees_where_text_goes() {
    let h = start(true).await;
    let view = device(&h, Permission::View).await;
    let info = view.clip_info().await.unwrap();
    assert!(info.enabled);
    assert_eq!(info.place, Place::Memory);
    assert_eq!(
        refusal(view.clip_put("hf_abc", true, true).await).0,
        ErrorCode::Forbidden
    );
    assert_eq!(refusal(view.clip_get().await).0, ErrorCode::Forbidden);
}

#[tokio::test(flavor = "multi_thread")]
async fn control_puts_but_only_shell_reads() {
    let h = start(true).await;
    let control = device(&h, Permission::Control).await;
    let shell = device(&h, Permission::Shell).await;

    control.clip_put("hf_abc", true, false).await.unwrap();
    assert_eq!(refusal(control.clip_get().await).0, ErrorCode::Forbidden);
    let got = shell.clip_get().await.unwrap();
    assert!(
        matches!(&got, ClipContent::Text { text, age_ms: Some(_) } if text == "hf_abc"),
        "{got:?}"
    );
    // Not `once`, so it is still there.
    assert_eq!(text(shell.clip_get().await.unwrap()), "hf_abc");

    assert_eq!(audited(&h, "clip_put"), [("memory".into(), "ok".into())]);
    assert_eq!(audited(&h, "clip_get").len(), 2);
    let entries = pocketgpu::audit::since(&h.agent.db(), 0).unwrap();
    assert!(entries.iter().all(|e| !e.target.contains("hf_abc")));
    assert!(entries.iter().all(|e| !e.result.contains("hf_abc")));
}

#[tokio::test(flavor = "multi_thread")]
async fn once_clears_after_the_first_read() {
    let h = start(true).await;
    let control = device(&h, Permission::Control).await;
    let shell = device(&h, Permission::Shell).await;

    control.clip_put("one-time", false, true).await.unwrap();
    assert_eq!(text(shell.clip_get().await.unwrap()), "one-time");
    assert_eq!(shell.clip_get().await.unwrap(), ClipContent::Empty);
    let mut results: Vec<_> = audited(&h, "clip_get").into_iter().map(|e| e.1).collect();
    results.sort();
    assert_eq!(results, ["empty", "ok"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn text_over_the_limit_is_refused() {
    let h = start(true).await;
    let control = device(&h, Permission::Control).await;
    let shell = device(&h, Permission::Shell).await;

    let (code, message) = refusal(
        control
            .clip_put(&"a".repeat(MAX_TEXT + 1), false, false)
            .await,
    );
    assert_eq!(code, ErrorCode::Invalid);
    assert!(message.contains("256 KB"), "{message}");
    assert_eq!(
        refusal(control.clip_put("", false, false).await).0,
        ErrorCode::Invalid
    );
    assert_eq!(shell.clip_get().await.unwrap(), ClipContent::Empty);

    let longest = "a".repeat(MAX_TEXT);
    control.clip_put(&longest, false, false).await.unwrap();
    assert_eq!(text(shell.clip_get().await.unwrap()).len(), MAX_TEXT);
    let local = local_request(
        &h.agent,
        Local::ClipPut {
            text: "a".repeat(MAX_TEXT + 1),
            once: false,
        },
    )
    .await;
    assert!(matches!(local, Reply::Error(e) if e.contains("256 KB")));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_line_shares_the_phones_clipboard() {
    let h = start(true).await;
    let control = device(&h, Permission::Control).await;
    let shell = device(&h, Permission::Shell).await;

    // `export HF_TOKEN=$(pgpu clip)` on the machine after the phone put it there.
    control
        .clip_put("hf_from_phone", false, false)
        .await
        .unwrap();
    let got = local_request(&h.agent, Local::ClipGet).await;
    assert!(
        matches!(&got, Reply::ClipText(Some(t)) if t == "hf_from_phone"),
        "{got:?}"
    );
    let status = local_request(&h.agent, Local::GetClip).await;
    assert!(
        matches!(&status, Reply::Clip(s) if s.place == Place::Memory
            && s.has_text
            && s.clears_in_s.is_some_and(|left| left > 0 && left <= 600)),
        "{status:?}"
    );

    // `tail -20 train.log | pgpu clip put --once`, then the phone reads it.
    let put = local_request(
        &h.agent,
        Local::ClipPut {
            text: "loss 0.12\n".into(),
            once: true,
        },
    )
    .await;
    assert!(matches!(&put, Reply::Clip(s) if s.has_text), "{put:?}");
    assert_eq!(text(shell.clip_get().await.unwrap()), "loss 0.12\n");
    assert!(matches!(
        local_request(&h.agent, Local::ClipGet).await,
        Reply::ClipText(None)
    ));

    control.clip_put("left over", false, false).await.unwrap();
    let cleared = local_request(&h.agent, Local::ClipClear).await;
    assert!(
        matches!(&cleared, Reply::Clip(s) if !s.has_text),
        "{cleared:?}"
    );
    assert_eq!(shell.clip_get().await.unwrap(), ClipContent::Empty);
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_keep_their_own_clipboards() {
    let a = start(true).await;
    let b = start(true).await;
    device(&a, Permission::Control)
        .await
        .clip_put("for a", false, false)
        .await
        .unwrap();
    assert_eq!(
        device(&b, Permission::Shell)
            .await
            .clip_get()
            .await
            .unwrap(),
        ClipContent::Empty
    );
}

/// `pgpu clip` over the local socket, as the command line runs it: the text, or exit 1.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn pgpu_clip_exits_1_when_empty() {
    use pocketgpu::cli::{Silent, clip};
    let h = start(false).await;
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    for _ in 0..250 {
        if h.agent.paths.socket().exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let code = |r: anyhow::Result<()>| match r {
        Ok(()) => 0,
        Err(e) => e.downcast_ref::<Silent>().expect("a plain exit code").0,
    };
    assert_eq!(code(clip::run(&h.agent.paths, false, None).await), 1);
    let get = Some(clip::Action::Get);
    assert_eq!(code(clip::run(&h.agent.paths, true, get).await), 1);
    let put = Local::ClipPut {
        text: "hf_abc".into(),
        once: false,
    };
    assert!(matches!(local_request(&h.agent, put).await, Reply::Clip(_)));
    assert_eq!(code(clip::run(&h.agent.paths, false, None).await), 0);
}
