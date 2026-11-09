//! F41 on Linux when the desktop's clipboard can't be opened: a display is named but nothing
//! answers there, so text goes to PocketGPU's own clipboard in memory, and Info says so.
//! Its own test binary, because it sets the environment.

#![cfg(target_os = "linux")]

mod harness;

use harness::Harness;
use pocketgpu::clip::{local_request, place};
use pocketgpu_client::ClipContent;
use pocketgpu_proto::Permission;
use pocketgpu_proto::clip::Place;
use pocketgpu_proto::local::{Request as Local, Response as Reply};

#[tokio::test(flavor = "multi_thread")]
async fn a_display_that_does_not_answer_falls_back_to_memory() {
    // SAFETY: the only test in this binary, and it sets these before starting anything.
    unsafe {
        std::env::set_var("DISPLAY", ":4711");
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("PGPU_CLIPBOARD");
    }
    assert_eq!(place(), Place::System);
    let h = Harness::start().await;
    local_request(&h.agent, Local::SetClip { on: true }).await;
    let control = h
        .connect(&h.phone(Some(Permission::Control)).await)
        .await
        .unwrap();
    let shell = h
        .connect(&h.phone(Some(Permission::Shell)).await)
        .await
        .unwrap();

    let info = control.clip_info().await.unwrap();
    assert_eq!(info.place, Place::Memory);
    assert_eq!(info.clears_after_s, Some(600));
    control.clip_put("hf_abc", true, true).await.unwrap();
    let status = local_request(&h.agent, Local::GetClip).await;
    assert!(
        matches!(&status, Reply::Clip(s) if s.place == Place::Memory && s.has_text),
        "{status:?}"
    );
    let got = shell.clip_get().await.unwrap();
    assert!(
        matches!(&got, ClipContent::Text { text, .. } if text == "hf_abc"),
        "{got:?}"
    );
    assert_eq!(shell.clip_get().await.unwrap(), ClipContent::Empty);

    let put = pocketgpu::audit::since(&h.agent.db(), 0)
        .unwrap()
        .into_iter()
        .find(|e| e.event == "clip_put")
        .unwrap();
    assert_eq!((put.target.as_str(), put.result.as_str()), ("memory", "ok"));
}
