//! Tests for [`super`]: what a round gets, where it lands, and how a folder is walked.

use super::*;

fn r(path: &str, size: u64, t: u64) -> Remote {
    Remote {
        path: path.into(),
        size,
        modified_ms: t,
    }
}

#[test]
fn new_and_changed_files_only() {
    let remote = [r("a.pt", 10, 1), r("b.pt", 20, 2), r("c/d.pt", 5, 3)];
    let mut known = Known::all(&remote[..2]);
    assert_eq!(needed(&remote, &known), vec![r("c/d.pt", 5, 3)]);
    known.0.get_mut("b.pt").unwrap().modified_ms = 1;
    let paths: Vec<_> = needed(&remote, &known)
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(paths, ["b.pt", "c/d.pt"]);
}

#[test]
fn nothing_leaves_the_folder() {
    assert!(safe("../x").is_none());
    assert!(safe("/etc/passwd").is_none());
    assert!(safe("a/../../x").is_none());
    assert!(safe(".pgpu-partial/x").is_none());
    assert!(safe("").is_none());
    assert_eq!(safe("runs/a.pt"), Some(PathBuf::from("runs/a.pt")));
    let remote = [r("../x", 1, 1), r("a.pt", 1, 1)];
    assert_eq!(needed(&remote, &Known::default()), vec![r("a.pt", 1, 1)]);
    assert_eq!(unsavable(&remote), 1);
}

/// Whether every part of `path` passes, as Windows (`true`) or Macs and Linux see it.
fn passes(path: &str, windows: bool) -> bool {
    path.split('/').all(|p| savable(p, windows))
}

#[test]
fn no_drives_streams_or_devices() {
    // Refused everywhere. On Windows `C:x` is a path on drive C:, not a file in the folder.
    for bad in [
        "a/C:x",
        "a/D:x",
        "C:x",
        "d:/x",
        r"a\..\..\x",
        "a/x\0y",
        "a/x\ny",
        "a//b",
        "a/./b",
        ".ssh/authorized_keys",
        "a/.zshrc",
        "a/photo\u{202E}gpj.exe",
    ] {
        assert!(!passes(bad, false), "{bad:?}");
        assert!(!passes(bad, true), "{bad:?}");
        assert_eq!(safe(bad), None, "{bad:?}");
    }
    // Refused on Windows only: streams, device names, trailing dots and spaces.
    for windows_only in [
        "a/x.txt:Zone.Identifier",
        "run-2026-11-20T10:00:00/a.pt",
        "a/what?.txt",
        "a/CON",
        "a/nul.txt",
        "a/COM1.log",
        "a/x.",
        "a/x ",
    ] {
        assert!(passes(windows_only, false), "{windows_only:?}");
        assert!(!passes(windows_only, true), "{windows_only:?}");
    }
    for ok in ["a/b c/d.tar.gz", "Résumé.pdf", "COM10", "runs/v1.2/a.pt"] {
        assert!(passes(ok, true) && safe(ok).is_some(), "{ok:?}");
    }
    let remote = [r("a/C:x", 1, 1), r("a/D:x", 1, 1)];
    assert!(needed(&remote, &Known::default()).is_empty());
}

#[test]
fn names_that_synced_before_still_do() {
    // Soft hyphens, zero-width spaces and names over 200 bytes (80 CJK characters).
    let long = format!("{}.png", "画".repeat(80));
    for ok in ["a/co\u{00AD}op.txt", "a/x\u{200B}y.txt", long.as_str()] {
        assert!(passes(ok, false) && passes(ok, true), "{ok:?}");
        assert!(safe(ok).is_some(), "{ok:?}");
    }
}

#[cfg(windows)]
#[test]
fn a_drive_never_replaces_the_folder() {
    let dir = Path::new(r"D:\Sync\runs");
    assert_eq!(within(dir, Path::new("C:x")), None);
    assert_eq!(within(dir, Path::new(r"a\D:x")), None);
    assert_eq!(
        within(dir, Path::new(r"a\b.txt")),
        Some(PathBuf::from(r"D:\Sync\runs\a\b.txt"))
    );
}

#[test]
fn joins_stay_in_the_folder() {
    let dir = Path::new("/sync/runs");
    assert_eq!(within(dir, Path::new("/etc/x")), None);
    assert_eq!(
        within(dir, Path::new("a/b.txt")),
        Some(PathBuf::from("/sync/runs/a/b.txt"))
    );
}

#[test]
fn what_came_before_times_here_were_kept_still_loads() {
    let old: Seen = serde_json::from_str(r#"{"size":5,"modified_ms":7}"#).unwrap();
    assert_eq!(old.here_ms, None);
    assert_eq!(
        serde_json::to_string(&old).unwrap(),
        r#"{"size":5,"modified_ms":7}"#
    );
}

/// How a file came with `size` bytes: no time here kept (it came before times were).
fn came(size: u64) -> Option<Seen> {
    Some(Seen {
        size,
        modified_ms: 1,
        here_ms: None,
    })
}

#[test]
fn a_file_changed_here_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let rel = Path::new("a.txt");
    assert_eq!(target(d.path(), rel, None), d.path().join("a.txt"));
    std::fs::write(d.path().join("a.txt"), b"12345").unwrap();
    // It came with 5 bytes and is still 5: the new version replaces it.
    assert_eq!(target(d.path(), rel, came(5)), d.path().join("a.txt"));
    // Changed here (or never came from the machine): the new one goes beside it.
    assert_eq!(target(d.path(), rel, came(3)), d.path().join("a (2).txt"));
    assert_eq!(target(d.path(), rel, None), d.path().join("a (2).txt"));
}

#[test]
fn an_edit_here_that_keeps_the_size_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let (rel, plain) = (Path::new("config.yaml"), d.path().join("config.yaml"));
    std::fs::write(&plain, b"lr: 0.001").unwrap();
    let then = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let file = std::fs::File::options().write(true).open(&plain).unwrap();
    file.set_modified(then).unwrap();
    drop(file);
    let landed = Some(Seen {
        size: 9,
        modified_ms: 1,
        here_ms: Some(1_700_000_000_000),
    });
    // As it landed: the next version replaces it.
    assert_eq!(target(d.path(), rel, landed), plain);
    // The same size, another time: edited here, so the next version goes beside it.
    std::fs::write(&plain, b"lr: 0.003").unwrap();
    assert_eq!(
        target(d.path(), rel, landed),
        d.path().join("config (2).yaml")
    );
}

fn entry(name: &str, kind: Kind, t: u64) -> Entry {
    Entry {
        name: name.into(),
        kind,
        size: 1,
        modified_ms: t,
    }
}

type Pages = std::collections::HashMap<(String, u32), (Vec<Entry>, Option<u32>)>;

/// A folder tree as the agent pages it: `(folder, offset)` to that page.
fn tree(pages: Vec<(&str, u32, Vec<Entry>, Option<u32>)>) -> Pages {
    pages
        .into_iter()
        .map(|(dir, offset, entries, next)| ((dir.to_string(), offset), (entries, next)))
        .collect()
}

async fn walked(pages: &Pages, max: usize) -> Scan {
    let list = |dir: String, offset| std::future::ready(Ok(pages[&(dir, offset)].clone()));
    walk(list, max).await.unwrap()
}

fn paths(scan: &Scan) -> Vec<&str> {
    scan.files.iter().map(|f| f.path.as_str()).collect()
}

#[tokio::test]
async fn the_newest_folders_come_first_so_a_full_scan_leaves_out_the_oldest() {
    use Kind::{File, Folder};
    // Folders first, newest first, over two pages, as the agent lists them.
    let pages = tree(vec![
        (
            "",
            0,
            vec![
                entry("2026-11-25", Folder, 300),
                entry("2026-06-01", Folder, 200),
            ],
            Some(2),
        ),
        ("", 2, vec![entry("2026-01-01", Folder, 100)], None),
        ("2026-11-25", 0, vec![entry("new.png", File, 3)], None),
        ("2026-06-01", 0, vec![entry("mid.png", File, 2)], None),
        ("2026-01-01", 0, vec![entry("old.png", File, 1)], None),
    ]);
    let all = walked(&pages, MAX_FILES).await;
    assert!(!all.full);
    assert_eq!(
        paths(&all),
        [
            "2026-11-25/new.png",
            "2026-06-01/mid.png",
            "2026-01-01/old.png"
        ]
    );
    let capped = walked(&pages, 2).await;
    assert!(capped.full);
    assert_eq!(paths(&capped), ["2026-11-25/new.png", "2026-06-01/mid.png"]);
}

#[tokio::test]
async fn an_entry_the_next_page_repeats_comes_once() {
    use Kind::{File, Folder};
    // A file came between pages, so the last entries of the first page come again.
    let pages = tree(vec![
        (
            "",
            0,
            vec![
                entry("runs", Folder, 9),
                entry("b.png", File, 5),
                entry("a.png", File, 4),
            ],
            Some(3),
        ),
        (
            "",
            3,
            vec![
                entry("runs", Folder, 9),
                entry("a.png", File, 4),
                entry("z.png", File, 1),
            ],
            None,
        ),
        ("runs", 0, vec![entry("1.pt", File, 2)], None),
    ]);
    let scan = walked(&pages, MAX_FILES).await;
    assert_eq!(paths(&scan), ["b.png", "a.png", "z.png", "runs/1.pt"]);
}
