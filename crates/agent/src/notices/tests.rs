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

#[test]
fn files_sent_here_and_clipboard_reads_show_at_fewer_too() {
    assert!(shown_at(NoticeLevel::Fewer, Kind::FileSent));
    assert!(shown_at(NoticeLevel::Fewer, Kind::ClipRead));
    assert!(!shown_at(NoticeLevel::Fewer, Kind::FileTaken));
    assert!(shown_at(NoticeLevel::All, Kind::FileTaken));

    let db = db_with_devices();
    let sent = remote(&db, "p8", Event::FilePut, "IMG_2041.jpg", "ok").unwrap();
    let n = text(&sent, "rig-1");
    assert_eq!(n.title, "Pixel 8 sent a file to this PC");
    assert_eq!(n.body, "IMG_2041.jpg is in the Inbox on rig-1.");
    // A get shows as it starts, so one that stops halfway shows too.
    let taken = remote(
        &db,
        "p8",
        Event::FileGet,
        "output/cat.png",
        crate::audit::STARTED,
    )
    .unwrap();
    assert_eq!(
        text(&taken, "rig-1").title,
        "Pixel 8 saved a file from this PC"
    );
    let read = remote(&db, "p8", Event::ClipGet, "memory", "ok").unwrap();
    assert_eq!(text(&read, "rig-1").title, "Pixel 8 read the clipboard");

    // Failed or empty ones, puts, and the command line's own show nothing.
    assert!(remote(&db, "p8", Event::FilePut, "a.bin", "size differs").is_none());
    let stopped = "stopped at 1.0 MB of 2.0 MB";
    assert!(remote(&db, "p8", Event::FileGet, "output/cat.png", stopped).is_none());
    assert!(remote(&db, "p8", Event::ClipGet, "memory", "empty").is_none());
    assert!(remote(&db, "p8", Event::ClipPut, "memory", "ok").is_none());
    assert!(remote(&db, "local", Event::FileOffer, "a.bin", "Pixel 8").is_none());
    assert!(remote(&db, "app", Event::FilePut, "a.bin", "ok").is_none());
}

#[test]
fn several_files_become_one_notice() {
    let db = db_with_devices();
    let mut w = Windows::new();
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    // Files wait for the rest of the batch: none shows at once.
    for (i, name) in ["a.jpg", "b.jpg", "c.jpg"].into_iter().enumerate() {
        let r = remote(&db, "p8", Event::FilePut, name, "ok").unwrap();
        assert!(group(&mut w, r, at(i as u64 * 4)).is_none());
    }
    // 10 s after the last one, the batch is one notice.
    assert!(flush(&mut w, at(17)).is_empty());
    let out = flush(&mut w, at(18));
    assert_eq!(out.len(), 1);
    let n = flushed(&out[0].0, out[0].1, "rig-1");
    assert_eq!(n.title, "Pixel 8 sent 3 files to this PC");
    assert!(n.body.starts_with("a.jpg and 2 more"), "{}", n.body);

    // A lone file gives its own notice, 10 s later.
    let r = remote(&db, "p8", Event::FilePut, "d.jpg", "ok").unwrap();
    assert!(group(&mut w, r, at(100)).is_none());
    let out = flush(&mut w, at(110));
    let n = flushed(&out[0].0, out[0].1, "rig-1");
    assert_eq!(n.title, "Pixel 8 sent a file to this PC");

    // A steady stream still shows a notice each minute.
    for s in (200..280).step_by(5) {
        let r = remote(&db, "p8", Event::FileGet, "output/x.png", "ok").unwrap();
        for (first, count) in flush(&mut w, at(s)) {
            assert_eq!((s, count), (260, 12), "{first:?}");
        }
        assert!(group(&mut w, r, at(s)).is_none());
    }

    let r = remote(&db, "p8", Event::ClipGet, "system", "ok").unwrap();
    assert_eq!(
        summary(&r, 4, "rig-1").title,
        "Pixel 8 read the clipboard 4 times"
    );
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

#[test]
fn guests_joining_and_ending_have_their_own_words() {
    let db = crate::store::db::open_in_memory().unwrap();
    crate::store::devices::upsert(
        &db,
        "g1",
        "Alex's Pixel",
        pocketgpu_proto::Platform::Android,
        pocketgpu_proto::Permission::Control,
        None,
    )
    .unwrap();
    let joined = remote(
        &db,
        "g1",
        Event::Pair,
        "Alex's Pixel",
        "control · guest for 8 hours · from Sam's iPhone",
    )
    .unwrap();
    let n = text(&joined, "rig-1");
    assert_eq!(n.title, "Alex's Pixel joined as a guest");
    assert!(
        n.body
            .starts_with("Invited by Sam's iPhone: Can run actions, for 8 hours."),
        "{}",
        n.body
    );
    assert!(n.revoke.is_some());
    let ended = remote(
        &db,
        "g1",
        Event::Revoke,
        "Alex's Pixel",
        crate::pair::guest::GUEST_ENDED,
    )
    .unwrap();
    assert_eq!(text(&ended, "rig-1").title, "Alex's Pixel’s access ended");
    assert!(shown_at(NoticeLevel::Fewer, Kind::GuestEnded));
}
