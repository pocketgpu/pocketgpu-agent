use std::time::{Duration, Instant};

use super::*;
use crate::store::db::open_in_memory;
use crate::store::devices;
use pocketgpu_proto::{Permission, Platform};

fn db_with_devices() -> Connection {
    let db = open_in_memory().unwrap();
    devices::upsert(
        &db,
        "p8",
        "Pixel 8",
        Platform::Android,
        Permission::Shell,
        None,
    )
    .unwrap();
    devices::upsert(
        &db,
        "app",
        "PocketGPU on this PC",
        Platform::Desktop,
        Permission::Shell,
        None,
    )
    .unwrap();
    devices::mark_this_pc(&db, "app").unwrap();
    db
}

#[test]
fn only_successful_remote_actions_and_never_this_pc_or_local() {
    let db = db_with_devices();
    let r = remote(&db, "p8", Event::ShellOpen, "session 1", "ok").unwrap();
    assert_eq!((r.name.as_str(), r.kind), ("Pixel 8", Kind::Terminal));
    assert!(remote(&db, "local", Event::ShellOpen, "session 1", "ok").is_none());
    assert!(remote(&db, "app", Event::ShellOpen, "session 1", "ok").is_none());
    assert!(
        remote(
            &db,
            "p8",
            Event::Kill,
            "pid 3",
            "process 3 is not using a GPU"
        )
        .is_none()
    );
    assert!(remote(&db, "p8", Event::Refused, "Kill", "view").is_none());
    assert!(remote(&db, "p8", Event::Revoke, "Pixel 8", "unpaired itself").is_none());
    assert_eq!(
        remote(&db, "p8", Event::ActionRun, "#3 Restart vLLM", "exit 0")
            .unwrap()
            .kind,
        Kind::Action
    );
    assert_eq!(
        remote(&db, "p8", Event::Revoke, "iPad", REVOKED_BY_DEVICE)
            .unwrap()
            .kind,
        Kind::Revoke
    );
}

#[test]
fn fewer_keeps_pairings_and_terminals() {
    assert!(shown_at(NoticeLevel::Fewer, Kind::Pair));
    assert!(shown_at(NoticeLevel::Fewer, Kind::Terminal));
    assert!(!shown_at(NoticeLevel::Fewer, Kind::Kill));
    assert!(shown_at(NoticeLevel::All, Kind::Container));
}

fn kill(n: u32) -> Remote {
    Remote {
        device: "p8".into(),
        name: "Pixel 8".into(),
        kind: Kind::Kill,
        target: format!("python train.py · pid {n}"),
        result: "sigterm sent".into(),
    }
}

#[test]
fn repeats_in_a_minute_become_one_summary() {
    let mut w = Windows::new();
    let t0 = Instant::now();
    assert!(group(&mut w, kill(1), t0).is_some());
    assert!(group(&mut w, kill(2), t0 + Duration::from_secs(10)).is_none());
    assert!(group(&mut w, kill(3), t0 + Duration::from_secs(20)).is_none());
    assert!(flush(&mut w, t0 + Duration::from_secs(30)).is_empty());
    let out = flush(&mut w, t0 + Duration::from_secs(61));
    assert_eq!(out.len(), 1);
    let n = summary(&out[0].0, out[0].1, "rig-1");
    assert_eq!(n.title, "Pixel 8 stopped 3 processes");
    assert!(n.body.starts_with("python train.py · pid 1 and 2 more"));
    // A lone action gives no summary.
    assert!(group(&mut w, kill(4), t0 + Duration::from_secs(70)).is_some());
    assert!(flush(&mut w, t0 + Duration::from_secs(140)).is_empty());
}

#[test]
fn terminals_and_pairings_are_never_grouped() {
    let mut w = Windows::new();
    let t0 = Instant::now();
    let mut r = kill(1);
    r.kind = Kind::Terminal;
    assert!(group(&mut w, r.clone(), t0).is_some());
    assert!(group(&mut w, r, t0 + Duration::from_secs(1)).is_some());
}

#[test]
fn texts_name_the_device_and_the_action() {
    let db = db_with_devices();
    let pair = remote(&db, "p8", Event::Pair, "Pixel 8", "shell").unwrap();
    let n = text(&pair, "rig-1");
    assert_eq!(n.title, "New device paired: Pixel 8");
    assert!(n.body.contains("Shell access"));
    assert_eq!(n.revoke, Some(("p8".into(), "Pixel 8".into())));
    let c = remote(
        &db,
        "p8",
        Event::Container,
        "restart 3f2a9c1b7d6e5f4a",
        "ok",
    )
    .unwrap();
    assert_eq!(
        text(&c, "rig-1").title,
        "Pixel 8 restarted container 3f2a9c1b7d6e"
    );
    let a = remote(&db, "p8", Event::ActionRun, "#3 Restart vLLM", "exit 0").unwrap();
    assert_eq!(text(&a, "rig-1").title, "Pixel 8 ran Restart vLLM");
    let t = remote(&db, "p8", Event::ShellOpen, "session 1", "ok").unwrap();
    assert_eq!(text(&t, "rig-1").title, "Pixel 8 opened a terminal");
    assert_eq!(text(&t, "rig-1").revoke, None);
}

#[test]
fn the_migration_marks_an_app_paired_before_it() {
    let db = open_in_memory().unwrap();
    crate::pair::local::pair(
        &db,
        &iroh::SecretKey::generate().public().to_string(),
        "PocketGPU on this PC",
    )
    .unwrap();
    let all = devices::list(&db).unwrap();
    assert!(all[0].this_pc);
}
