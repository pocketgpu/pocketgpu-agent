//! Pairing end to end: `pgpu pair` session ↔ agent ↔ phone, on one machine.

mod harness;

use harness::Harness;
use pocketgpu_client::{ClientError, Pairing, PhoneInfo};
use pocketgpu_proto::local::{PairCodeInfo, Request, Response};
use pocketgpu_proto::msg::pair;
use pocketgpu_proto::pairing::{PairCode, PairInput};
use pocketgpu_proto::{ErrorCode, Permission, Platform, read_frame, write_frame};
use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

/// The command line's end of a `pgpu pair` session.
struct Cli {
    r: ReadHalf<DuplexStream>,
    w: WriteHalf<DuplexStream>,
    info: PairCodeInfo,
}

impl Cli {
    async fn start(h: &Harness) -> Self {
        let (cli, agent_side) = tokio::io::duplex(64 * 1024);
        let (ar, aw) = tokio::io::split(agent_side);
        tokio::spawn(pocketgpu::pair::session(h.agent.clone(), ar, aw));
        let (mut r, w) = tokio::io::split(cli);
        let Some(Response::PairCode(info)) = read_frame(&mut r).await.unwrap() else {
            panic!("expected a pairing code first");
        };
        Self { r, w, info }
    }

    async fn next(&mut self) -> Response {
        read_frame(&mut self.r)
            .await
            .unwrap()
            .expect("session ended")
    }

    async fn decide(&mut self, permission: Option<Permission>) {
        write_frame(&mut self.w, &Request::PairDecision { permission })
            .await
            .unwrap();
    }

    fn input(&self) -> PairInput {
        PairInput::parse(&self.info.qr).unwrap()
    }
}

fn info() -> PhoneInfo {
    PhoneInfo {
        device_name: "iPhone 16".into(),
        platform: Platform::Ios,
        push_ticket: Some("ticket-1".into()),
    }
}

async fn start_pair(h: &Harness, phone: &harness::Phone, code: PairCode) -> Pairing {
    phone
        .client
        .pair_at(h.router.endpoint().addr(), code, None, info())
        .await
        .unwrap()
}

#[tokio::test]
async fn qr_pairing_with_approval() {
    let h = Harness::start().await;
    let mut cli = Cli::start(&h).await;
    assert_eq!(cli.info.code.len(), 9);
    let PairInput::Qr(qr) = cli.input() else {
        panic!("the QR payload should parse");
    };
    assert_eq!(qr.agent_id, h.agent.identity.agent_id().to_string());

    let phone = h.phone(None).await;
    let mut pairing = start_pair(&h, &phone, qr.code).await;
    let pair::Response::Pending { agent_name, .. } = pairing.next().await.unwrap() else {
        panic!("expected Pending");
    };
    assert_eq!(agent_name, h.agent.name());
    assert_eq!(pairing.agent_id(), Some(h.agent.identity.agent_id()));

    let Response::PairAsk(ask) = cli.next().await else {
        panic!("expected PairAsk");
    };
    assert_eq!(ask.device_name, "iPhone 16");
    cli.decide(Some(Permission::Control)).await;

    assert_eq!(
        pairing.next().await.unwrap(),
        pair::Response::Approved {
            permission: Permission::Control,
            relays: vec![],
        }
    );
    assert!(matches!(
        cli.next().await,
        Response::Paired {
            permission: Permission::Control,
            ..
        }
    ));
    assert_eq!(
        h.agent.permission_of(&phone.key.public()),
        Some(Permission::Control)
    );
    assert_eq!(h.audit_events("pair"), 1);

    // The new phone can now use the machine.
    let machine = h.connect(&phone).await.unwrap();
    let info = machine.hello("test").await.unwrap();
    assert_eq!(info.permission, Permission::Control);
}

#[tokio::test]
async fn typed_code_finds_the_rendezvous_endpoint() {
    let h = Harness::start().await;
    let mut cli = Cli::start(&h).await;
    let code = PairCode::parse(&cli.info.code).unwrap();
    let addr = h
        .agent
        .pairing
        .rendezvous_addr()
        .expect("rendezvous endpoint");
    assert_eq!(addr.id, pocketgpu_client::rendezvous_id(&code));

    let phone = h.phone(None).await;
    let mut pairing = phone
        .client
        .pair_at(addr, code, None, info())
        .await
        .unwrap();
    assert!(matches!(
        pairing.next().await.unwrap(),
        pair::Response::Pending { .. }
    ));
    // A typed code learns the machine's real ID from the reply.
    assert_eq!(pairing.agent_id(), Some(h.agent.identity.agent_id()));
    assert!(matches!(cli.next().await, Response::PairAsk(_)));
    cli.decide(Some(Permission::View)).await;
    assert!(matches!(
        pairing.next().await.unwrap(),
        pair::Response::Approved {
            permission: Permission::View,
            ..
        }
    ));
}

#[tokio::test]
async fn denied_phone_is_not_paired() {
    let h = Harness::start().await;
    let mut cli = Cli::start(&h).await;
    let phone = h.phone(None).await;
    let mut pairing = start_pair(&h, &phone, cli.input().code()).await;
    pairing.next().await.unwrap();
    assert!(matches!(cli.next().await, Response::PairAsk(_)));
    cli.decide(None).await;
    assert_eq!(pairing.next().await.unwrap(), pair::Response::Denied);
    assert!(matches!(cli.next().await, Response::PairEnded(_)));
    assert_eq!(h.agent.permission_of(&phone.key.public()), None);
}

#[tokio::test]
async fn wrong_codes_are_refused_then_burned() {
    let h = Harness::start().await;
    let mut cli = Cli::start(&h).await;
    let right = cli.input().code();
    let wrong = PairCode::parse(if right.as_str() == "00000000" {
        "11111111"
    } else {
        "00000000"
    })
    .unwrap();

    for _ in 0..pocketgpu::pair::code::MAX_WRONG {
        let phone = h.phone(None).await;
        let mut pairing = start_pair(&h, &phone, wrong).await;
        match pairing.next().await {
            Err(ClientError::Remote(e)) => assert_eq!(e.code, ErrorCode::Forbidden),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    assert!(matches!(cli.next().await, Response::PairEnded(_)));
    assert!(!h.agent.pairing.is_open());

    // Even the right code no longer works.
    let phone = h.phone(None).await;
    let result = match phone
        .client
        .pair_at(h.router.endpoint().addr(), right, None, info())
        .await
    {
        Ok(mut p) => p.next().await.map(|_| ()),
        Err(e) => Err(e),
    };
    assert!(result.is_err());
    assert!(h.audit_events("refused") >= 5);
}

#[tokio::test]
async fn code_works_once() {
    let h = Harness::start().await;
    let mut cli = Cli::start(&h).await;
    let code = cli.input().code();
    let first = h.phone(None).await;
    let mut pairing = start_pair(&h, &first, code).await;
    pairing.next().await.unwrap();
    assert!(matches!(cli.next().await, Response::PairAsk(_)));

    // While the first phone waits for approval, nobody else may use the code.
    let second = h.phone(None).await;
    let result = match second
        .client
        .pair_at(h.router.endpoint().addr(), code, None, info())
        .await
    {
        Ok(mut p) => p.next().await.map(|_| ()),
        Err(e) => Err(e),
    };
    assert!(result.is_err());
}

#[tokio::test]
async fn pairing_is_closed_without_an_active_code() {
    let h = Harness::start().await;
    let phone = h.phone(None).await;
    let code = PairCode::parse("4F7K92QD").unwrap();
    let result = match phone
        .client
        .pair_at(h.router.endpoint().addr(), code, None, info())
        .await
    {
        Ok(mut p) => p.next().await.map(|_| ()),
        Err(e) => Err(e),
    };
    assert!(result.is_err());
    assert_eq!(h.agent.permission_of(&phone.key.public()), None);
}

#[tokio::test]
async fn qr_for_another_machine_is_rejected() {
    let h = Harness::start().await;
    let mut cli = Cli::start(&h).await;
    let phone = h.phone(None).await;
    let impostor = iroh::SecretKey::generate().public();
    let mut pairing = phone
        .client
        .pair_at(
            h.router.endpoint().addr(),
            cli.input().code(),
            Some(impostor),
            info(),
        )
        .await
        .unwrap();
    // The proof was bound to the real endpoint, but the phone expected another key.
    match pairing.next().await {
        Err(ClientError::WrongMachine) => {}
        other => panic!("expected WrongMachine, got {other:?}"),
    }
    drop(pairing);
    // The session asks anyway; denying ends it.
    if let Response::PairAsk(_) = cli.next().await {
        cli.decide(None).await;
    }
}

#[tokio::test]
async fn only_one_pairing_at_a_time() {
    let h = Harness::start().await;
    let _first = Cli::start(&h).await;
    let (cli, agent_side) = tokio::io::duplex(4096);
    let (ar, aw) = tokio::io::split(agent_side);
    tokio::spawn(pocketgpu::pair::session(h.agent.clone(), ar, aw));
    let (mut r, _w) = tokio::io::split(cli);
    assert!(matches!(
        read_frame::<_, Response>(&mut r).await.unwrap(),
        Some(Response::Error(_))
    ));
}
