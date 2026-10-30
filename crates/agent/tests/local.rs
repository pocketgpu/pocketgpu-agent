//! The tray app's local client finds and talks to the agent like the command line does.
#![cfg(unix)]

mod harness;

use harness::Harness;
use pocketgpu_proto::local::{Request, Response};

#[cfg(unix)]
#[test]
fn finds_the_same_socket_as_the_agent() {
    let paths = pocketgpu::store::paths::Paths::discover().unwrap();
    assert_eq!(pocketgpu_client::local::socket_path(), Some(paths.socket()));
    assert_eq!(pocketgpu_client::local::data_dir(), Some(paths.data_dir));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn asks_the_agent_over_its_socket() {
    let h = Harness::start().await;
    let dir = h.agent.paths.data_dir.clone();
    // SAFETY: this test binary sets no other environment and runs this test alone.
    unsafe { std::env::set_var("PGPU_DATA_DIR", &dir) };
    tokio::spawn(pocketgpu::ipc::listen(h.agent.clone()));
    let mut reply = None;
    for _ in 0..50 {
        if let Ok(r) = pocketgpu_client::local::call(&Request::Status).await {
            reply = Some(r);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        matches!(reply, Some(Response::Status(ref s)) if s.name == h.agent.name()),
        "{reply:?}"
    );
}
