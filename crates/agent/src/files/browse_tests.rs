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
