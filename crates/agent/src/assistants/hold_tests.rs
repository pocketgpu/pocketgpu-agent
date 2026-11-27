//! The running agent's side of held actions: device checks, the audit, the settings and
//! the guard file (T015).

use std::sync::Arc;

use pocketgpu_proto::assistants::{Answer, AskWhy, Ended};
use pocketgpu_proto::{Permission, Platform};
use tokio::sync::oneshot::error::TryRecvError;

use super::*;
use crate::assistants::approvals::{COULDNT_ASK, Held, NewRequest, Opened};

const T0: i64 = 1_000_000;

fn new(cmd: &str) -> NewRequest {
    NewRequest {
        why: AskWhy::AskFirst,
        kind: pocketgpu_proto::assistants::Kind::Delete,
        assistant: "claude".into(),
        session: "s1".into(),
        permission_mode: "default".into(),
        tool: "Bash".into(),
        detail_text: cmd.into(),
        input_hash: crate::assistants::approvals::input_hash("Bash", cmd),
        folder: "/work".into(),
        summary: cmd.into(),
    }
}

fn held(o: Opened) -> Held {
    match o {
        Opened::Held(h) => h,
        Opened::Ended(a, _) => panic!("not held: {a:?}"),
    }
}

fn waiting(h: &mut Held) -> bool {
    matches!(h.rx.try_recv(), Err(TryRecvError::Empty))
}

fn agent() -> Arc<crate::daemon::state::Agent> {
    let gpus = Arc::new(crate::collect::fake::FakeGpus::new(1));
    crate::daemon::state::Agent::in_memory(gpus).unwrap()
}

#[test]
fn a_revoked_lowered_or_guest_device_is_refused() {
    use crate::store::devices;
    let agent = agent();
    {
        let db = agent.db();
        devices::upsert(
            &db,
            "full",
            "Pixel",
            Platform::Android,
            Permission::Shell,
            None,
        )
        .unwrap();
        devices::upsert(
            &db,
            "low",
            "Old",
            Platform::Android,
            Permission::Control,
            None,
        )
        .unwrap();
        devices::upsert(
            &db,
            "guest",
            "Guest",
            Platform::Ios,
            Permission::Shell,
            None,
        )
        .unwrap();
        devices::set_expiry(&db, "guest", Some(T0 * 10_000)).unwrap();
    }
    let ap = &agent.assistants.approvals;
    let mut h = held(ap.open(new("rm x"), T0));
    for dev in ["low", "guest", "gone"] {
        assert!(answer(&agent, dev, h.id, T0, true).is_err(), "{dev}");
    }
    assert!(waiting(&mut h));
    // Full control, then lowered: refused from then on.
    devices::set_permission(&agent.db(), "full", Permission::Control).unwrap();
    assert!(answer(&agent, "full", h.id, T0, true).is_err());
    devices::set_permission(&agent.db(), "full", Permission::Shell).unwrap();
    let view = answer(&agent, "full", h.id, T0, true).unwrap();
    assert_eq!(view.answered_by_name.as_deref(), Some("Pixel"));
    assert_eq!(h.rx.try_recv().unwrap(), Answer::Allow);
    // Revoked: refused.
    let mut h2 = held(ap.open(new("rm y"), T0));
    devices::remove(&agent.db(), "full").unwrap();
    assert!(answer(&agent, "full", h2.id, T0, true).is_err());
    assert!(waiting(&mut h2));
    // The answer was audited once, naming the phone.
    let rows = crate::audit::since(&agent.db(), 0).unwrap();
    let req: Vec<_> = rows
        .iter()
        .filter(|e| e.event == "assistant_request")
        .collect();
    assert_eq!(req.len(), 1);
    assert_eq!(
        (req[0].device.as_str(), req[0].result.as_str()),
        ("full", "allowed")
    );
}

#[tokio::test]
async fn the_agent_holds_only_what_matches() {
    let agent = agent();
    let call = |why, mode: &str, cmd: &str, oversize| Call {
        why,
        assistant: "claude".into(),
        session: "s1".into(),
        permission_mode: mode.into(),
        tool: "Bash".into(),
        input: serde_json::json!({ "command": cmd }).to_string(),
        oversize,
        folder: "/w".into(),
    };
    // Off: everything passes.
    for why in [AskWhy::AskFirst, AskWhy::AskOnPhone] {
        assert!(matches!(
            start(&agent, call(why, "default", "rm x", false)),
            First::Answer(Answer::Pass)
        ));
    }
    let mut cfg = crate::store::config::AssistantConfig {
        ask_first: vec!["delete".into()],
        ..Default::default()
    };
    save_settings(&agent, cfg.clone(), "local").unwrap();
    assert!(
        crate::assistants::guard::armed(&agent.paths),
        "the guard follows the settings"
    );
    assert!(matches!(
        start(&agent, call(AskWhy::AskFirst, "default", "ls", false)),
        First::Answer(Answer::Pass)
    ));
    assert!(matches!(
        start(&agent, call(AskWhy::AskFirst, "default", "rm x", false)),
        First::Held(_)
    ));
    // Oversize or too long for the phone: never held, never passed.
    assert_eq!(
        match start(&agent, call(AskWhy::AskFirst, "auto", "ls", true)) {
            First::Answer(a) => a,
            First::Held(_) => panic!("held"),
        },
        Answer::Deny {
            message: COULDNT_ASK.into()
        }
    );
    let long = format!("rm {}", "x".repeat(pocketgpu_proto::assistants::DETAIL_MAX));
    assert!(matches!(
        start(&agent, call(AskWhy::AskFirst, "default", &long, false)),
        First::Answer(Answer::NoDecision {
            why: Ended::NoPhone
        })
    ));
    cfg.ask_first.clear();
    save_settings(&agent, cfg, "local").unwrap();
    assert!(!crate::assistants::guard::armed(&agent.paths));
    let rows = crate::audit::since(&agent.db(), 0).unwrap();
    assert!(rows.iter().any(|e| e.event == "assistant_settings"));
}

fn bash_call(why: AskWhy, cmd: &str) -> Call {
    Call {
        why,
        assistant: "claude".into(),
        session: "s1".into(),
        permission_mode: "default".into(),
        tool: "Bash".into(),
        input: serde_json::json!({ "command": cmd }).to_string(),
        oversize: false,
        folder: "/w".into(),
    }
}

/// Starts a call that must be held and allows it as a phone would.
fn hold_and_allow(agent: &Arc<crate::daemon::state::Agent>, why: AskWhy, cmd: &str) {
    let First::Held(h) = start(agent, bash_call(why, cmd)) else {
        panic!("not held: {cmd}");
    };
    let ap = &agent.assistants.approvals;
    let opened = ap.get(h.id).unwrap().opened_ms;
    ap.answer(h.id, opened, "d", "Pixel", true, opened).unwrap();
}

fn is_allow(f: &First) -> bool {
    matches!(f, First::Answer(Answer::Allow))
}

#[tokio::test]
async fn an_ask_on_phone_allow_is_never_reused() {
    let agent = agent();
    let cfg = crate::store::config::AssistantConfig {
        ask_on_phone: true,
        ..Default::default()
    };
    save_settings(&agent, cfg, "local").unwrap();
    hold_and_allow(&agent, AskWhy::AskOnPhone, "git push --force");
    // The same command again in the same session asks the phone again.
    let again = start(&agent, bash_call(AskWhy::AskOnPhone, "git push --force"));
    assert!(matches!(again, First::Held(_)), "{again:?}");
}

#[tokio::test]
async fn a_second_run_needs_its_own_allow() {
    let agent = agent();
    let cfg = crate::store::config::AssistantConfig {
        ask_on_phone: true,
        ask_first: vec!["delete".into()],
        ..Default::default()
    };
    save_settings(&agent, cfg, "local").unwrap();
    // Run 1: allowed on the phone, its desk prompt never came.
    hold_and_allow(&agent, AskWhy::AskFirst, "rm -rf build");
    // Run 2: held again, and reaches no phone.
    let First::Held(h) = start(&agent, bash_call(AskWhy::AskFirst, "rm -rf build")) else {
        panic!("run 2 not held");
    };
    end(&agent, h.id, Ended::NoPhone);
    // Its desk prompt must not get run 1's Allow.
    let ask = start(&agent, bash_call(AskWhy::AskOnPhone, "rm -rf build"));
    assert!(!is_allow(&ask), "{ask:?}");
}

#[tokio::test]
async fn a_disarmed_check_forgets_the_allow() {
    let agent = agent();
    let mut cfg = crate::store::config::AssistantConfig {
        ask_first: vec!["delete".into()],
        ..Default::default()
    };
    save_settings(&agent, cfg.clone(), "local").unwrap();
    hold_and_allow(&agent, AskWhy::AskFirst, "rm x");
    cfg.ask_first.clear();
    save_settings(&agent, cfg, "local").unwrap();
    assert!(matches!(
        start(&agent, bash_call(AskWhy::AskFirst, "rm x")),
        First::Answer(Answer::Pass)
    ));
    assert!(matches!(
        start(&agent, bash_call(AskWhy::AskOnPhone, "rm x")),
        First::Answer(Answer::Pass)
    ));
}

#[tokio::test]
async fn the_desk_prompt_after_a_phone_allow_is_allowed_once() {
    let agent = agent();
    // Only "Ask me first": a user's ask rule still raises the desk prompt after the Allow.
    let cfg = crate::store::config::AssistantConfig {
        ask_first: vec!["delete".into()],
        ..Default::default()
    };
    save_settings(&agent, cfg, "local").unwrap();
    hold_and_allow(&agent, AskWhy::AskFirst, "rm -rf build");
    assert!(is_allow(&start(
        &agent,
        bash_call(AskWhy::AskOnPhone, "rm -rf build")
    )));
    assert!(matches!(
        start(&agent, bash_call(AskWhy::AskOnPhone, "rm -rf build")),
        First::Answer(Answer::Pass)
    ));
}
