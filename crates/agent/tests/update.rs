//! A phone updates the agent (board 23): only with Shell, and only where the agent can
//! restart itself. No network here: every case is refused before anything is downloaded.

mod harness;

use harness::Harness;
use pocketgpu_client::ClientError;
use pocketgpu_proto::msg::control::{Request, Response};
use pocketgpu_proto::{ErrorCode, Permission};

fn refused(r: Result<Response, ClientError>) -> (ErrorCode, String) {
    match r {
        Err(ClientError::Remote(e)) => (e.code, e.message),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn only_shell_phones_may_install_an_update() {
    let h = Harness::start().await;
    let install = || Request::InstallUpdate {
        version: "99.0.0".into(),
    };
    for p in [Permission::View, Permission::Control] {
        let phone = h.phone(Some(p)).await;
        let machine = h.connect(&phone).await.unwrap();
        assert_eq!(
            refused(machine.control(install()).await).0,
            ErrorCode::Forbidden
        );
    }

    let shell = h.phone(Some(Permission::Shell)).await;
    let machine = h.connect(&shell).await.unwrap();
    let (code, _) = refused(
        machine
            .control(Request::InstallUpdate {
                version: "latest; rm -rf ~".into(),
            })
            .await,
    );
    assert_eq!(code, ErrorCode::Invalid);
    // The test agent isn't a service (or runs in a container), so it couldn't restart into
    // the new version: refused with a reason.
    let (code, why) = refused(machine.control(install()).await);
    assert_eq!(code, ErrorCode::Busy, "{why}");
    assert!(
        why.contains("pgpu update") || why.contains("container"),
        "{why}"
    );
}
