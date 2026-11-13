//! F42 over a real connection: apps are off until turned on at the machine, only Full
//! control (Shell) opens one, a browser's request goes through the local address to the app
//! on the machine and back, and opening is audited once, not per connection.

mod harness;

use harness::Harness;
use pocketgpu::apps::local_request;
use pocketgpu_client::{ClientError, Machine};
use pocketgpu_proto::local::{Request as Local, Response as Reply};
use pocketgpu_proto::{ErrorCode, Permission};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A tiny web app on this machine: answers every request with its title and the path.
async fn web_app() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split(' ').nth(1).unwrap_or("/").to_string();
                let body = format!("<title>Gradio</title>{path}");
                let reply = format!(
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    port
}

async fn add(h: &Harness, port: u16) {
    let reply = local_request(&h.agent, Local::AddApp { port, name: None }).await;
    assert!(matches!(reply, Reply::Apps(_)), "{reply:?}");
}

async fn turn(h: &Harness, on: bool) {
    let reply = local_request(&h.agent, Local::SetApps { on }).await;
    assert!(
        matches!(&reply, Reply::Apps(a) if a.enabled == on),
        "{reply:?}"
    );
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

async fn get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

fn opened(h: &Harness) -> Vec<String> {
    pocketgpu::audit::since(&h.agent.db(), 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.event == "app_open")
        .map(|e| e.target)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn off_until_turned_on_and_only_full_control_opens() {
    let h = Harness::start().await;
    let port = web_app().await;
    add(&h, port).await;
    let view = device(&h, Permission::View).await;
    let control = device(&h, Permission::Control).await;
    let shell = device(&h, Permission::Shell).await;

    // Off: nothing is listed, and opening says how to turn it on.
    let list = view.apps().await.unwrap();
    assert!(!list.enabled && list.apps.is_empty());
    let (code, message) = refusal(shell.forward_app(port, Some(0)).await);
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(message.contains("pgpu apps on"), "{message}");

    turn(&h, true).await;
    let list = view.apps().await.unwrap();
    let app = list.apps.iter().find(|a| a.port == port).expect("found");
    assert_eq!(app.name, "Gradio app");
    assert_eq!(
        refusal(view.forward_app(port, Some(0)).await).0,
        ErrorCode::Forbidden
    );
    assert_eq!(
        refusal(control.forward_app(port, Some(0)).await).0,
        ErrorCode::Forbidden
    );

    // A port nobody added is refused, even with Full control.
    let (code, _) = refusal(
        shell
            .forward_app(port.wrapping_add(1).max(1), Some(0))
            .await,
    );
    assert_eq!(code, ErrorCode::Forbidden);

    // Hidden at the machine: listed, but not opened.
    let reply = local_request(&h.agent, Local::SetAppOff { port, off: true }).await;
    assert!(matches!(&reply, Reply::Apps(a) if a.apps.iter().any(|x| x.port == port && x.off)));
    assert_eq!(
        refusal(shell.forward_app(port, Some(0)).await).0,
        ErrorCode::Forbidden
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_reaches_the_app_through_the_local_address() {
    let h = Harness::start().await;
    let port = web_app().await;
    add(&h, port).await;
    turn(&h, true).await;
    let shell = device(&h, Permission::Shell).await;

    let fwd = shell.forward_app(port, Some(0)).await.unwrap();
    assert!(fwd.local.ip().is_loopback());
    for path in ["/", "/lab", "/api/kernels"] {
        let page = get(fwd.local, path).await;
        assert!(page.ends_with(path), "{page}");
    }

    // Several connections, one audit entry (and one notice).
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let entries = opened(&h);
    assert_eq!(entries, vec![format!("Gradio app :{port}")]);

    // Dropped: the local address stops answering.
    let local = fwd.local;
    drop(fwd);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(TcpStream::connect(local).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn jupyter_opens_with_its_token_and_others_at_the_start() {
    let runtime = tempfile::tempdir().unwrap();
    // SAFETY: the only test here that reads Jupyter's folder; set before anything starts.
    unsafe { std::env::set_var("JUPYTER_RUNTIME_DIR", runtime.path()) };
    let h = Harness::start().await;
    let jupyter = web_app().await;
    let other = web_app().await;
    add(&h, jupyter).await;
    add(&h, other).await;
    let server = format!(r#"{{"port": {jupyter}, "token": "f00d", "base_url": "/"}}"#);
    std::fs::write(runtime.path().join("jpserver-42.json"), server).unwrap();

    let shell = device(&h, Permission::Shell).await;
    let view = device(&h, Permission::View).await;
    // Off: refused like Open.
    assert_eq!(
        refusal(shell.app_start(jupyter).await).0,
        ErrorCode::Forbidden
    );
    turn(&h, true).await;
    assert_eq!(shell.app_start(jupyter).await.unwrap(), "/?token=f00d");
    assert_eq!(shell.app_start(other).await.unwrap(), "/");
    // The token goes only to a device that may open the app.
    assert_eq!(
        refusal(view.app_start(jupyter).await).0,
        ErrorCode::Forbidden
    );
}
