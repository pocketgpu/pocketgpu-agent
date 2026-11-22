//! F48 end to end: a Shell device shares one model; one browser key claims it once; that key
//! may chat with that model only, within the owner's limits, and is refused on every other
//! protocol; stopping ends it at once; nothing said is kept anywhere.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::harness;

use harness::Harness;
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, RelayMode, SecretKey};
use pocketgpu_client::ClientError;
use pocketgpu_proto::pairing::proof;
use pocketgpu_proto::share::{
    AgentToGuest, ChatMsg, GuestRefusal, GuestState, GuestToAgent, ModelServerRef, NewShare, Role,
    ShareInfo, ShareLink, ShareState,
};
use pocketgpu_proto::{ErrorCode, Permission, alpn, read_frame, write_frame};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The word a guest asks about; it must never be kept.
const SECRET_WORD: &str = "pineapple-umbrella-42";

/// A tiny Ollama with one model that streams a three-part answer.
async fn fake_ollama() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.starts_with("GET /api/tags") {
                    r#"{"models":[{"name":"llama3:8b"}]}"#.to_string()
                } else if req.starts_with("POST /api/chat") {
                    ["Hel", "lo ", "there"]
                        .iter()
                        .map(|t| format!("{{\"message\":{{\"role\":\"assistant\",\"content\":\"{t}\"}},\"done\":false}}\n"))
                        .chain(std::iter::once("{\"done\":true}\n".to_string()))
                        .collect()
                } else {
                    r#"{"error":"not found"}"#.to_string()
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(body.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}

fn new_share(limit: Option<u32>) -> NewShare {
    NewShare {
        server: ModelServerRef::Ollama,
        model: "llama3:8b".into(),
        label: "Mum".into(),
        from: Some("Kami".into()),
        minutes: Some(10080),
        daily_limit: limit,
        pause_for_jobs: true,
    }
}

/// An agent with the fake Ollama and a relay set (links need one), and an owner with Shell.
async fn setup() -> (Harness, pocketgpu_client::Machine) {
    let h = Harness::start().await;
    let ollama = fake_ollama().await;
    {
        let mut cfg = h.agent.config.write().unwrap();
        cfg.ollama_addr = ollama;
        cfg.relays = vec!["https://relay.example.test/".into()];
    }
    let owner = h.phone(Some(Permission::Shell)).await;
    let m = h.connect(&owner).await.unwrap();
    (h, m)
}

async fn guest_conn(h: &Harness, key: &SecretKey, alpn: &[u8]) -> Connection {
    let ep = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(key.clone())
        .bind()
        .await
        .unwrap();
    let conn = ep.connect(h.router.endpoint().addr(), alpn).await.unwrap();
    // A dropped endpoint closes its connections; this one lives as long as the test.
    std::mem::forget(ep);
    conn
}

/// Sends one request and reads every reply on its stream.
async fn ask(conn: &Connection, req: GuestToAgent) -> Vec<AgentToGuest> {
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open_bi: {e}");
            return vec![];
        }
    };
    if let Err(e) = write_frame(&mut send, &req).await {
        eprintln!("write: {e}");
        return vec![];
    }
    let _ = send.finish();
    let mut out = vec![];
    loop {
        match read_frame::<_, AgentToGuest>(&mut recv).await {
            Ok(Some(m)) => out.push(m),
            Ok(None) => break,
            Err(e) => {
                eprintln!("read: {e} (close: {:?})", conn.close_reason());
                break;
            }
        }
    }
    out
}

fn link_of(h: &Harness, s: &ShareInfo) -> ShareLink {
    s.link
        .clone()
        .unwrap()
        .link(*h.agent.identity.agent_id().as_bytes())
}

fn claim_req(link: &ShareLink, key: &SecretKey) -> GuestToAgent {
    GuestToAgent::Claim {
        share: link.share,
        proof: proof(&link.secret, &link.computer, key.public().as_bytes()),
    }
}

fn chat(text: &str) -> GuestToAgent {
    GuestToAgent::Chat {
        messages: vec![ChatMsg {
            role: Role::User,
            text: text.into(),
        }],
    }
}

fn refusal(replies: &[AgentToGuest]) -> Option<GuestRefusal> {
    replies.iter().find_map(|r| match r {
        AgentToGuest::Refused(x) => Some(*x),
        _ => None,
    })
}

fn code<T>(r: Result<T, ClientError>) -> ErrorCode {
    match r {
        Err(ClientError::Remote(e)) => e.code,
        Ok(_) => panic!("expected a refusal"),
        Err(e) => panic!("expected a refusal, got {e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_shell_shares_and_only_offered_choices() {
    let (h, owner) = setup().await;
    let controller = h.phone(Some(Permission::Control)).await;
    let c = h.connect(&controller).await.unwrap();
    assert_eq!(
        code(c.create_share(new_share(Some(200))).await),
        ErrorCode::Forbidden
    );
    assert_eq!(code(c.shares().await), ErrorCode::Forbidden);
    let odd = NewShare {
        daily_limit: Some(7),
        ..new_share(None)
    };
    assert_eq!(code(owner.create_share(odd).await), ErrorCode::Invalid);
    let gone = NewShare {
        model: "nope:1b".into(),
        ..new_share(None)
    };
    assert_eq!(code(owner.create_share(gone).await), ErrorCode::NotFound);

    let s = owner.create_share(new_share(Some(200))).await.unwrap();
    assert_eq!(s.state, ShareState::Waiting);
    let url = owner.share_url(&s).unwrap();
    let frag = url.split('#').nth(1).unwrap();
    assert_eq!(ShareLink::from_fragment(frag).unwrap(), link_of(&h, &s));
    assert_eq!(h.audit_events("share"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_browser_claims_and_chats_with_that_model_only() {
    let (h, owner) = setup().await;
    let s = owner.create_share(new_share(Some(200))).await.unwrap();
    let link = link_of(&h, &s);

    // A wrong proof is refused; the right one claims.
    let key = SecretKey::generate();
    let conn = guest_conn(&h, &key, alpn::MODEL_GUEST).await;
    let wrong = GuestToAgent::Claim {
        share: link.share,
        proof: [0; 32],
    };
    assert_eq!(
        refusal(&ask(&conn, wrong).await),
        Some(GuestRefusal::NotForYou)
    );
    let claimed = ask(&conn, claim_req(&link, &key)).await;
    let Some(AgentToGuest::Claimed(st)) = claimed.first() else {
        panic!("{claimed:?}");
    };
    assert_eq!(st.model, "llama3:8b");
    assert_eq!(st.from.as_deref(), Some("Kami"));
    assert_eq!(st.state, GuestState::Ready);

    // Chat streams the model's answer and counts one message.
    let replies = ask(&conn, chat("hello")).await;
    let text: String = replies
        .iter()
        .filter_map(|r| match r {
            AgentToGuest::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello there");
    assert_eq!(replies.last(), Some(&AgentToGuest::Done { used_today: 1 }));

    // A second browser can't use the link.
    let other = SecretKey::generate();
    let c2 = guest_conn(&h, &other, alpn::MODEL_GUEST).await;
    assert_eq!(
        refusal(&ask(&c2, claim_req(&link, &other)).await),
        Some(GuestRefusal::AlreadyClaimed)
    );
    // And a key that never claimed gets nothing.
    let stranger = SecretKey::generate();
    let c3 = guest_conn(&h, &stranger, alpn::MODEL_GUEST).await;
    assert_eq!(
        refusal(&ask(&c3, chat("hi")).await),
        Some(GuestRefusal::NotForYou)
    );

    // The owner sees counts.
    let list = owner.shares().await.unwrap();
    assert_eq!(list[0].state, ShareState::Active);
    assert_eq!(list[0].used_today, 1);
    assert!(
        list[0].link.is_none(),
        "an active share's link isn't shown again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_key_is_refused_on_every_other_protocol() {
    let (h, owner) = setup().await;
    let s = owner.create_share(new_share(None)).await.unwrap();
    let link = link_of(&h, &s);
    let key = SecretKey::generate();
    let conn = guest_conn(&h, &key, alpn::MODEL_GUEST).await;
    assert!(matches!(
        ask(&conn, claim_req(&link, &key)).await.first(),
        Some(AgentToGuest::Claimed(_))
    ));

    for proto in alpn::ALL {
        // The two pairing protocols are open to unknown keys by design and grant nothing
        // without a code or a yes at the computer (their own tests).
        if [alpn::MODEL_GUEST, alpn::PAIR, alpn::PAIR_LAN].contains(&proto) {
            continue;
        }
        let c = guest_conn(&h, &key, proto).await;
        let closed = tokio::time::timeout(Duration::from_secs(5), c.closed()).await;
        assert!(
            closed.is_ok(),
            "{} kept a guest key's connection open",
            String::from_utf8_lossy(proto)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_pause_and_stop_hold() {
    let (h, owner) = setup().await;
    let s = owner.create_share(new_share(Some(50))).await.unwrap();
    let link = link_of(&h, &s);
    let key = SecretKey::generate();
    let conn = guest_conn(&h, &key, alpn::MODEL_GUEST).await;
    ask(&conn, claim_req(&link, &key)).await;

    // The day's limit.
    {
        let db = h.agent.db();
        let day = pocketgpu::share::day::local_day(pocketgpu::store::db::now_ms());
        db.execute(
            "INSERT INTO model_usage (share_id, day, messages) VALUES (?1, ?2, 50)",
            rusqlite::params![s.id as i64, day],
        )
        .unwrap();
    }
    assert!(matches!(
        refusal(&ask(&conn, chat("one more")).await),
        Some(GuestRefusal::LimitReached { .. })
    ));
    h.agent.db().execute("DELETE FROM model_usage", []).unwrap();

    // A running job pauses it.
    h.agent
        .db()
        .execute(
            "INSERT INTO jobs (name, command, cwd, started_at, state, log_path) \
             VALUES ('train', 'python train.py', '/tmp', 0, 'running', '/tmp/x.log')",
            [],
        )
        .unwrap();
    assert_eq!(
        refusal(&ask(&conn, chat("hi")).await),
        Some(GuestRefusal::Paused)
    );
    h.agent.db().execute("DELETE FROM jobs", []).unwrap();
    assert!(
        ask(&conn, chat("hi"))
            .await
            .iter()
            .any(|r| matches!(r, AgentToGuest::Done { .. }))
    );

    // Stop: the connection closes within 10 s and the key is refused after.
    let started = std::time::Instant::now();
    owner.stop_share(s.id).await.unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await;
    assert!(closed.is_ok(), "the guest stayed connected after Stop");
    assert!(started.elapsed() < Duration::from_secs(10));
    let again = guest_conn(&h, &key, alpn::MODEL_GUEST).await;
    assert_eq!(
        refusal(&ask(&again, chat("hi")).await),
        Some(GuestRefusal::NotForYou)
    );
}

/// Collects everything the agent logs, at every level, on this thread's runtime.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn nothing_said_is_kept_or_logged() {
    let logs = Captured::default();
    let sink = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || sink.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    // Proves the capture is attached to this test's runtime, so "not in the log" means something.
    tracing::info!("capture works");

    let (h, owner) = setup().await;
    let s = owner.create_share(new_share(None)).await.unwrap();
    let link = link_of(&h, &s);
    let key = SecretKey::generate();
    let conn = guest_conn(&h, &key, alpn::MODEL_GUEST).await;
    ask(&conn, claim_req(&link, &key)).await;
    let replies = ask(&conn, chat(&format!("what is a {SECRET_WORD}?"))).await;
    assert!(
        replies
            .iter()
            .any(|r| matches!(r, AgentToGuest::Done { .. }))
    );

    let logged = String::from_utf8_lossy(&logs.0.lock().unwrap()).to_string();
    assert!(
        logged.contains("capture works"),
        "the log capture saw nothing"
    );
    assert!(!logged.contains(SECRET_WORD), "a guest's words were logged");
    assert!(!logged.contains("Hello there"), "an answer was logged");

    let db = h.agent.db();
    let mut tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap();
    let names: Vec<String> = tables
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for t in names {
        let dump: String = db
            .query_row(
                &format!(
                    "SELECT COALESCE(group_concat(quote(t.*)), '') FROM (SELECT * FROM \"{t}\") t"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        assert!(
            !dump.contains(SECRET_WORD),
            "a guest's words were stored in {t}"
        );
    }
}
