use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use super::*;

// The countdown tests call `schedule` directly: `request` also asks this host's system what
// it allows (logind on Linux), which differs between machines and CI runners.
use crate::collect::fake::FakeGpus;

static RAN: AtomicU32 = AtomicU32::new(0);
fn fake_run(_: PowerKind) -> Result<(), String> {
    RAN.fetch_add(1, Ordering::SeqCst);
    Ok(())
}
fn refused(_: PowerKind) -> Result<(), String> {
    Err("Access denied".into())
}

fn agent() -> Arc<Agent> {
    Agent::in_memory(Arc::new(FakeGpus::new(1))).unwrap()
}

#[tokio::test]
async fn off_until_turned_on_here() {
    let a = agent();
    let e = request(
        &a,
        "p",
        "Pixel 8",
        PowerKind::Restart,
        Duration::ZERO,
        fake_run,
    )
    .unwrap_err();
    assert!(e.contains("pgpu power on"), "{e}");
    set_enabled(&a, true).unwrap();
    assert!(state(&a).enabled);
}

#[tokio::test]
async fn counts_down_then_runs_and_one_at_a_time() {
    let a = agent();
    set_enabled(&a, true).unwrap();
    let before = RAN.load(Ordering::SeqCst);
    let wait = Duration::from_millis(60);
    schedule(&a, "p", "Pixel 8", PowerKind::Restart, wait, fake_run).unwrap();
    assert!(a.power.pending().is_some());
    let e = schedule(&a, "p", "Pixel 8", PowerKind::ShutDown, wait, fake_run).unwrap_err();
    assert!(e.contains("already counting down"), "{e}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(RAN.load(Ordering::SeqCst), before + 1);
    let last = a.power.last().unwrap();
    assert_eq!(
        (last.by.as_str(), last.result.as_str()),
        ("Pixel 8", "started")
    );
    assert!(a.power.pending().is_none());
}

#[tokio::test]
async fn cancelled_here_never_runs() {
    let a = agent();
    set_enabled(&a, true).unwrap();
    schedule(
        &a,
        "p",
        "Pixel 8",
        PowerKind::Sleep,
        Duration::from_millis(80),
        refused,
    )
    .unwrap();
    assert!(a.power.cancel(&a));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.power.last().unwrap().result, "cancelled");
    assert!(!a.power.cancel(&a));
}

#[tokio::test]
async fn a_refusal_is_kept_in_words() {
    let a = agent();
    set_enabled(&a, true).unwrap();
    schedule(
        &a,
        "p",
        "Pixel 8",
        PowerKind::ShutDown,
        Duration::from_millis(20),
        refused,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(a.power.last().unwrap().result, "Access denied");
}

#[test]
fn reads_what_the_system_says() {
    assert_eq!(
        os::parse_busctl_string("s \"challenge\"\n").as_deref(),
        Some("challenge")
    );
    let on = "\r\nHKEY_LOCAL_MACHINE\\\\...\\\\Winlogon\r\n    AutoAdminLogon    REG_SZ    1\r\n";
    assert_eq!(
        os::auto_sign_in(on),
        pocketgpu_proto::msg::control::ComesBack::Yes
    );
    assert_eq!(
        os::auto_sign_in("    AutoAdminLogon    REG_SZ    0\r\n"),
        pocketgpu_proto::msg::control::ComesBack::NoAutoSignIn
    );
}
