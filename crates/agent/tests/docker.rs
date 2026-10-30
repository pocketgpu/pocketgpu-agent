//! F28 over a real connection: Docker stays off until `pgpu docker on`, View lists and reads
//! logs, Control starts and stops. The Docker parts run only where Docker answers.

mod harness;

use std::time::Duration;

use harness::Harness;
use pocketgpu_client::ClientError;
use pocketgpu_proto::docker::Action;
use pocketgpu_proto::{ErrorCode, Permission};

fn code<T: std::fmt::Debug>(r: Result<T, ClientError>) -> ErrorCode {
    match r {
        Err(ClientError::Remote(e)) => e.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_is_off_until_turned_on_at_the_machine() {
    let h = Harness::start().await;
    let control = h
        .connect(&h.phone(Some(Permission::Control)).await)
        .await
        .unwrap();
    assert_eq!(code(control.containers().await), ErrorCode::Forbidden);
    assert_eq!(
        code(control.container_action("pgpu-demo", Action::Stop).await),
        ErrorCode::Forbidden
    );

    pocketgpu::docker::local(&h.agent, Some(true)).await;
    let view = h
        .connect(&h.phone(Some(Permission::View)).await)
        .await
        .unwrap();
    // Starting and stopping needs Control, whatever Docker says.
    assert_eq!(
        code(view.container_action("pgpu-demo", Action::Restart).await),
        ErrorCode::Forbidden
    );
    assert_eq!(
        code(control.container_action("../images", Action::Stop).await),
        ErrorCode::Invalid
    );

    let list = match view.containers().await {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Docker isn't reachable here, skipping the rest: {e}");
            return;
        }
    };
    let Some(running) = list.iter().find(|c| c.state == "running") else {
        eprintln!("no running container here, skipping logs");
        return;
    };
    let mut log = view.container_logs(&running.id, 5).await.unwrap();
    let lines = tokio::time::timeout(Duration::from_secs(10), log.next())
        .await
        .expect("log lines in time")
        .unwrap()
        .expect("some lines");
    assert!(!lines.is_empty());
    println!("{} ({}): {:?}", running.name, running.image, lines);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_protocol_the_agent_lacks_reads_as_too_old() {
    // An older agent has no handler for a newer feature's protocol; the phone says to update.
    let h = Harness::start().await;
    let phone = h.phone(Some(Permission::View)).await;
    let err = phone
        .client
        .dial(h.router.endpoint().addr(), b"pgpu/not-yet/1")
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Unsupported), "got {err:?}");
}
