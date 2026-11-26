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

/// A paired phone's key: only a device that still has Control gets its countdown run.
fn paired(a: &Agent, name: &str, permission: Permission) -> String {
    use crate::store::devices;
    let key = iroh::SecretKey::generate().public().to_string();
    let p = pocketgpu_proto::Platform::Android;
    devices::upsert(&a.db(), &key, name, p, permission, None).unwrap();
    key
}

/// The audit's results for power.
fn power_results(a: &Agent) -> Vec<String> {
    let all = crate::audit::since(&a.db(), 0).unwrap();
    let power = all.into_iter().filter(|e| e.event == "power");
    power.map(|e| e.result).collect()
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
    let p = paired(&a, "Pixel 8", Permission::Control);
    let before = RAN.load(Ordering::SeqCst);
    let wait = Duration::from_millis(60);
    schedule(&a, &p, "Pixel 8", PowerKind::Restart, wait, fake_run).unwrap();
    assert!(a.power.pending().is_some());
    let e = schedule(&a, &p, "Pixel 8", PowerKind::ShutDown, wait, fake_run).unwrap_err();
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
    let p = paired(&a, "Pixel 8", Permission::Control);
    schedule(
        &a,
        &p,
        "Pixel 8",
        PowerKind::ShutDown,
        Duration::from_millis(20),
        refused,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(a.power.last().unwrap().result, "Access denied");
}

/// The command succeeded but nothing went down: on a Mac an app with unsaved work stops it.
fn ran_but_stayed_on(_: PowerKind) -> Result<(), String> {
    Ok(())
}

#[tokio::test]
async fn still_on_later_says_so() {
    let a = agent();
    set_enabled(&a, true).unwrap();
    let p = paired(&a, "Pixel 8", Permission::Control);
    let wait = Duration::from_millis(20);
    let run = ran_but_stayed_on;
    schedule(&a, &p, "Pixel 8", PowerKind::Restart, wait, run).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(a.power.last().unwrap().result, "started");
    tokio::time::sleep(STILL_UP).await;
    assert_eq!(a.power.last().unwrap().result, STILL_ON);
    assert!(power_results(&a).iter().any(|r| r == STILL_ON));
    // A Mac that slept is still running when it wakes: that's no sign it didn't.
    schedule(&a, &p, "Pixel 8", PowerKind::Sleep, wait, run).unwrap();
    tokio::time::sleep(STILL_UP + Duration::from_millis(300)).await;
    assert_eq!(a.power.last().unwrap().result, "started");
}

// `refused` stands for the system's command below: had it run, the outcome would read
// "Access denied" instead of "cancelled".

#[tokio::test]
async fn revoked_at_the_computer_never_runs() {
    let a = agent();
    set_enabled(&a, true).unwrap();
    let p = paired(&a, "Guest phone", Permission::Control);
    let wait = Duration::from_millis(80);
    schedule(&a, &p, "Guest phone", PowerKind::ShutDown, wait, refused).unwrap();
    crate::daemon::devices::revoke(&a, "Guest phone").unwrap();
    assert!(
        a.power.pending().is_none(),
        "the countdown ends with the device"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.power.last().unwrap().result, "cancelled");
    assert!(power_results(&a).iter().any(|r| r == ASKER_GONE));
}

#[tokio::test]
async fn a_guest_whose_time_ends_never_runs() {
    let a = agent();
    set_enabled(&a, true).unwrap();
    let p = paired(&a, "Guest phone", Permission::Control);
    let wait = Duration::from_millis(80);
    schedule(&a, &p, "Guest phone", PowerKind::Restart, wait, refused).unwrap();
    crate::store::devices::set_expiry(&a.db(), &p, Some(now_ms() - 1)).unwrap();
    assert_eq!(crate::pair::guest::sweep(&a), 1);
    assert!(a.power.pending().is_none());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.power.last().unwrap().result, "cancelled");
}

#[tokio::test]
async fn checked_again_when_the_countdown_ends() {
    // Its time ran out, or it was set to View, without anything cancelling the countdown.
    for lost in ["expired", "view"] {
        let a = agent();
        set_enabled(&a, true).unwrap();
        let p = paired(&a, "Guest phone", Permission::Control);
        let wait = Duration::from_millis(60);
        schedule(&a, &p, "Guest phone", PowerKind::Sleep, wait, refused).unwrap();
        if lost == "expired" {
            crate::store::devices::set_expiry(&a.db(), &p, Some(now_ms() - 1)).unwrap();
        } else {
            crate::store::devices::set_permission(&a.db(), &p, Permission::View).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(a.power.last().unwrap().result, "cancelled", "{lost}");
        assert!(power_results(&a).iter().any(|r| r == ASKER_GONE), "{lost}");
    }
}

#[tokio::test]
async fn another_device_removed_leaves_the_countdown() {
    let a = agent();
    set_enabled(&a, true).unwrap();
    let p = paired(&a, "Pixel 8", Permission::Control);
    paired(&a, "Old tablet", Permission::Control);
    let wait = Duration::from_millis(500);
    schedule(&a, &p, "Pixel 8", PowerKind::Restart, wait, refused).unwrap();
    crate::daemon::devices::revoke(&a, "Old tablet").unwrap();
    assert!(a.power.pending().is_some());
    assert!(a.power.cancel(&a));
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
