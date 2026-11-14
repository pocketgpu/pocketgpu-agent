//! F45 over a real connection, with the made-up screen: seeing the screen is off until
//! turned on at the machine, only Full control (Shell) gets pictures, a look is listed while
//! it lasts and someone at the machine can stop it, turning it off stops everyone, and the
//! first look from a device goes in the activity log once.

mod harness;

use std::time::Duration;

use harness::Harness;
use pocketgpu::screen::local_request;
use pocketgpu_client::screen::Seen;
use pocketgpu_client::{ClientError, Machine};
use pocketgpu_proto::local::{Request as Local, Response as Reply, ScreenStatus};
use pocketgpu_proto::{ErrorCode, Permission};

fn fake_screen() {
    static SET: std::sync::Once = std::sync::Once::new();
    // SAFETY: set once, before any thread of this test reads it.
    SET.call_once(|| unsafe { std::env::set_var("PGPU_FAKE_SCREEN", "1") });
}

async fn local(h: &Harness, req: Local) -> ScreenStatus {
    match local_request(&h.agent, req).await {
        Reply::Screen(s) => s,
        other => panic!("{other:?}"),
    }
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

fn looks(h: &Harness) -> usize {
    pocketgpu::audit::since(&h.agent.db(), 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.event == "screen_look")
        .count()
}

/// The next thing the look sends, within a few seconds.
async fn next(w: &mut pocketgpu_client::screen::Watch) -> Option<Seen> {
    tokio::time::timeout(Duration::from_secs(10), w.next())
        .await
        .expect("the look sent nothing")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_screen_from_a_phone() {
    fake_screen();
    let h = Harness::start().await;
    let shell = device(&h, Permission::Shell).await;
    let viewer = device(&h, Permission::View).await;

    // Off: nothing about the displays, and no pictures.
    let info = shell.screen_info().await.unwrap();
    assert!(!info.enabled && info.displays.is_empty());
    let (code, msg) = refusal(shell.screen_peek(0, 640).await);
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(msg.contains("pgpu screen on"), "{msg}");

    assert!(local(&h, Local::SetScreen { on: true }).await.enabled);
    let info = viewer.screen_info().await.unwrap();
    assert_eq!(info.displays.len(), 2);
    // Looking needs Full control.
    assert_eq!(
        refusal(viewer.screen_peek(0, 640).await).0,
        ErrorCode::Forbidden
    );

    let f = shell.screen_peek(0, 640).await.unwrap();
    assert_eq!((f.width, f.height), (640, 360));
    assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8]);
    assert_eq!(shell.screen_peek(1, 4000).await.unwrap().width, 1024);

    // A look is listed while it lasts, and someone here can stop it.
    let mut w = shell.screen_watch(0, 800, 8).await.unwrap();
    match next(&mut w).await {
        Some(Seen::Frame(f)) => assert_eq!(f.width, 800),
        other => panic!("{other:?}"),
    }
    let s = local(&h, Local::GetScreen).await;
    assert_eq!(s.watching.len(), 1);
    local(&h, Local::StopScreen { device: None }).await;
    loop {
        match next(&mut w).await {
            Some(Seen::Stopped) => break,
            Some(_) => {}
            None => panic!("ended without Stopped"),
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(local(&h, Local::GetScreen).await.watching.is_empty());

    // Turning it off stops everyone looking.
    let mut w = shell.screen_watch(1, 800, 2).await.unwrap();
    assert!(matches!(next(&mut w).await, Some(Seen::Frame(_))));
    local(&h, Local::SetScreen { on: false }).await;
    loop {
        match next(&mut w).await {
            Some(Seen::Stopped) => break,
            Some(_) => {}
            None => panic!("ended without Stopped"),
        }
    }

    // Peeks and looks from one device: one entry (and one notice).
    assert_eq!(looks(&h), 1);
}
