use super::*;

fn entry(name: &str, kind: Kind, modified_ms: u64) -> Entry {
    Entry {
        name: name.into(),
        kind,
        size: 0,
        modified_ms,
    }
}

fn names(r: &Response) -> (Vec<String>, Option<u32>) {
    match r {
        Response::Entries { entries, next } => {
            (entries.iter().map(|e| e.name.clone()).collect(), *next)
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn folders_first_then_newest() {
    let all = vec![
        entry("old.png", Kind::File, 1),
        entry("b", Kind::Folder, 1),
        entry("new.png", Kind::File, 9),
        entry("a", Kind::Folder, 1),
    ];
    let (got, next) = names(&page(all, 0));
    assert_eq!(got, ["a", "b", "new.png", "old.png"]);
    assert_eq!(next, None);
}

#[test]
fn pages_of_500() {
    let all: Vec<_> = (0..1200)
        .map(|i| entry(&format!("{i:04}"), Kind::File, i))
        .collect();
    let (first, next) = names(&page(all.clone(), 0));
    assert_eq!((first.len(), next), (500, Some(500)));
    assert_eq!(first[0], "1199");
    let (last, next) = names(&page(all.clone(), 1000));
    assert_eq!((last.len(), next), (200, None));
    let (past, next) = names(&page(all, 5000));
    assert!(past.is_empty() && next.is_none());
}

#[test]
fn job_logs_are_numbered_log_files() {
    assert!(is_job_log("12.log"));
    assert!(!is_job_log(".log"));
    assert!(!is_job_log("12.log.1"));
    assert!(!is_job_log("../12.log"));
    assert!(!is_job_log("a12.log"));
    let dir = tempfile::Builder::new()
        .prefix("pgpu-jobs-")
        .tempdir()
        .unwrap();
    std::fs::write(dir.path().join("3.log"), "x").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
    std::fs::create_dir(dir.path().join("4.log")).unwrap();
    assert_eq!(job_logs(dir.path()), vec![dir.path().join("3.log")]);
}

#[cfg(unix)]
#[test]
fn listings_skip_hidden_names_and_links_out() {
    let dir = tempfile::Builder::new()
        .prefix("pgpu-list-")
        .tempdir()
        .unwrap();
    let root = dir.path().join("shared");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a.png"), "abc").unwrap();
    std::fs::write(root.join(".env"), "secret").unwrap();
    std::fs::write(dir.path().join("out.txt"), "no").unwrap();
    std::os::unix::fs::symlink(dir.path().join("out.txt"), root.join("out")).unwrap();
    std::os::unix::fs::symlink(root.join("a.png"), root.join("same.png")).unwrap();
    let rules = Rules::new(&root).unwrap();
    let mut got: Vec<_> = read_folder(&rules, &rules.root)
        .into_iter()
        .map(|e| (e.name, e.kind, e.size))
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("a.png".to_string(), Kind::File, 3),
            ("same.png".to_string(), Kind::File, 3),
            ("sub".to_string(), Kind::Folder, 0),
        ]
    );
}

#[cfg(unix)]
#[test]
fn listings_leave_out_closed_folders_and_links_to_them() {
    let dir = tempfile::Builder::new()
        .prefix("pgpu-list-")
        .tempdir()
        .unwrap();
    let root = dir.path().join("Downloads");
    let inbox = root.join("PocketGPU/Inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("owner.pdf"), "private").unwrap();
    std::fs::write(root.join("PocketGPU/notes.txt"), "ok").unwrap();
    std::os::unix::fs::symlink(&inbox, root.join("sent")).unwrap();
    let mut rules = Rules::new(&root).unwrap();
    rules.deny = vec![inbox.canonicalize().unwrap()];
    let names = |dir: &Path| {
        let mut n: Vec<_> = read_folder(&rules, dir)
            .into_iter()
            .map(|e| e.name)
            .collect();
        n.sort();
        n
    };
    assert_eq!(names(&rules.root), ["PocketGPU"]);
    assert_eq!(names(&rules.root.join("PocketGPU")), ["notes.txt"]);
}

/// On a disk that ignores capitals (`pocketgpu/inbox` on Linux keeps the spelling asked
/// for) or through a bind mount, a path to the Inbox doesn't start with the Inbox's own
/// path: it stays closed by which folder it is on disk.
#[cfg(unix)]
#[test]
fn closed_folders_stay_closed_however_a_path_spells_them() {
    let dir = tempfile::Builder::new()
        .prefix("pgpu-spelt-")
        .tempdir()
        .unwrap();
    let root = dir.path().join("Downloads");
    let inbox = root.join("PocketGPU/Inbox");
    std::fs::create_dir_all(inbox.join("old")).unwrap();
    std::fs::write(inbox.join("owner.pdf"), "private").unwrap();
    std::fs::write(root.join("PocketGPU/notes.txt"), "ok").unwrap();
    std::os::unix::fs::symlink(&inbox, root.join("sent")).unwrap();
    let mut rules = Rules::new(&root).unwrap();
    // The path that was set doesn't match the one asked for.
    rules.deny = vec![dir.path().join("downloads/pocketgpu/inbox")];
    rules.deny_ids = FileId::of(&std::fs::metadata(&inbox).unwrap())
        .into_iter()
        .collect();
    let closed = |r: Result<_, Refused>| matches!(r, Err(Refused::Outside));
    assert!(closed(rules.open("PocketGPU/Inbox/owner.pdf").map(|_| ())));
    assert!(closed(rules.folder("PocketGPU/Inbox/old").map(|_| ())));
    assert!(closed(rules.folder("sent").map(|_| ())));
    assert!(rules.open("PocketGPU/notes.txt").is_ok());
    let names = |dir: &Path| {
        let mut n: Vec<_> = read_folder(&rules, dir)
            .into_iter()
            .map(|e| e.name)
            .collect();
        n.sort();
        n
    };
    assert_eq!(names(&rules.root), ["PocketGPU"]);
    assert_eq!(names(&rules.root.join("PocketGPU")), ["notes.txt"]);
    // A shared folder inside the Inbox is closed too.
    let mut inside = Rules::new(&inbox.join("old")).unwrap();
    inside.deny_ids = rules.deny_ids.clone();
    assert!(closed(inside.folder("").map(|_| ())));
    let held = Closed {
        paths: Vec::new(),
        ids: rules.deny_ids.clone(),
    };
    assert!(held.holds(&inbox.join("old").canonicalize().unwrap()));
    assert!(!held.holds(&rules.root));
}

/// Looking at the Inbox can wait on macOS asking to allow Downloads: a guest's request waits
/// for that only once, a moment, and no second look starts while one is going.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_look_at_the_inbox_is_waited_for_once() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::{Duration, Instant};
    static LOOKS: AtomicUsize = AtomicUsize::new(0);
    fn slow(dir: &Path) -> Found {
        LOOKS.fetch_add(1, SeqCst);
        std::thread::sleep(Duration::from_millis(1600));
        Some((dir.to_path_buf(), None))
    }
    let looker = Looker::new();
    let dir = PathBuf::from("/nowhere/Inbox");
    let started = Instant::now();
    assert_eq!(looker.find(dir.clone(), slow).await, None);
    assert!(started.elapsed() < Duration::from_millis(1500));
    let again = Instant::now();
    assert_eq!(looker.find(dir.clone(), slow).await, None);
    assert!(again.elapsed() < Duration::from_millis(300));
    tokio::time::sleep(Duration::from_millis(900)).await;
    // Back now: its answer is used.
    let found = Some(Some((dir.clone(), None)));
    assert_eq!(looker.find(dir, slow).await, found);
    assert_eq!(LOOKS.load(SeqCst), 1);
}

/// Requests at the same time get the Inbox as it is, not "not looked at yet".
#[tokio::test(flavor = "multi_thread")]
async fn requests_at_the_same_time_all_get_the_inbox() {
    fn quick(dir: &Path) -> Found {
        std::thread::sleep(std::time::Duration::from_millis(50));
        Some((dir.to_path_buf(), None))
    }
    let looker = Looker::new();
    let dir = PathBuf::from("/nowhere/Inbox");
    let (a, b, c) = tokio::join!(
        looker.find(dir.clone(), quick),
        looker.find(dir.clone(), quick),
        looker.find(dir.clone(), quick)
    );
    let found = Some(Some((dir, None)));
    assert_eq!([a, b, c], [found.clone(), found.clone(), found]);
}
