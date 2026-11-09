use super::*;

fn inbox() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("pgpu-inbox-")
        .tempdir()
        .unwrap();
    let inbox = dir.path().join("Inbox");
    std::fs::create_dir_all(inbox.join(PARTIAL_DIR)).unwrap();
    (dir, inbox)
}

fn partial(inbox: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = inbox.join(PARTIAL_DIR).join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

#[test]
fn partials_are_named_by_device_transfer_and_size() {
    let name = partial_name("k3xq9ab7longerkey", &[0xab; 16], 600);
    assert_eq!(name, format!("k3xq9ab7-{}-600.part", "ab".repeat(16)));
    // Only letters and digits of the key, whatever it holds.
    assert!(partial_name("../../x", &[0; 16], 1).starts_with("x-"));
    let key = transfer_key("k3xq9ab7longerkey", &[0xab; 16]);
    assert_eq!(key_of(&name), Some(key.as_str()));
    assert_eq!(key_of(&name.replace(".part", ".done")), Some(key.as_str()));
    assert_eq!(key_of("notes.txt"), None);
}

#[cfg(unix)]
#[test]
fn saved_files_are_private_and_never_executable() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, inbox) = inbox();
    let p = partial(&inbox, "a-1-10.part", b"#!/bin/sh\n");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (name, got) = finish(&p, &inbox, "run.sh", 10, &hash(b"#!/bin/sh\n"))
        .unwrap()
        .unwrap();
    assert_eq!(got, hash(b"#!/bin/sh\n"));
    let mode = std::fs::metadata(inbox.join(name))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_partial_of_the_wrong_size_is_not_saved() {
    let (_dir, inbox) = inbox();
    let p = partial(&inbox, "a-1-10.part", b"short");
    assert!(finish(&p, &inbox, "a.bin", 10, &hash(b"short")).is_err());
    assert!(!inbox.join("a.bin").exists());
}

#[test]
fn bytes_that_are_not_the_devices_file_are_never_placed() {
    let (_dir, inbox) = inbox();
    // Half of the old file, then the rest of a new one of the same size.
    let p = partial(&inbox, "a-1-6.part", b"oldNEW");
    let done = finish(&p, &inbox, "photo.jpg", 6, &hash(b"newNEW")).unwrap();
    assert_eq!(done, None);
    assert!(!inbox.join("photo.jpg").exists());
    // The partial goes, so the next try starts over.
    assert!(!p.exists());
}

#[test]
fn a_finished_send_leaves_a_note_of_its_name() {
    let (_dir, inbox) = inbox();
    let p = partial(&inbox, "a-1-3.part", b"abc");
    let (name, got) = finish(&p, &inbox, "a.txt", 3, &hash(b"abc"))
        .unwrap()
        .unwrap();
    let note = p.with_extension("done");
    assert_eq!(read_note(&note), Some((name, got)));
    std::fs::write(&note, "not a note").unwrap();
    assert_eq!(read_note(&note), None);
    assert_eq!(read_note(&inbox.join("missing.done")), None);
}

#[test]
fn old_partials_and_notes_are_removed() {
    let (_dir, inbox) = inbox();
    let old = partial(&inbox, "a-1-1.part", b"x");
    let old_note = partial(&inbox, "a-3-1.done", b"x");
    let new = partial(&inbox, "a-2-1.part", b"x");
    let other = partial(&inbox, "notes.txt", b"x");
    let day_ago = SystemTime::now() - PARTIAL_AGE - Duration::from_secs(60);
    for path in [&old, &old_note, &other] {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(day_ago)
            .unwrap();
    }
    assert_eq!(remove_old_partials(&inbox, SystemTime::now()), 2);
    assert!(!old.exists() && !old_note.exists() && new.exists() && other.exists());
    // No Inbox yet: nothing to do.
    assert_eq!(
        remove_old_partials(&inbox.join("missing"), SystemTime::now()),
        0
    );
}

#[test]
fn a_moved_inbox_keeps_no_partials() {
    let (_dir, inbox) = inbox();
    let idle = partial(&inbox, "idle-1-1.part", b"x");
    let note = partial(&inbox, "idle-2-1.done", b"x");
    let busy = partial(&inbox, "busy-1-1.part", b"x");
    let writing = Writing::take("busy-1").unwrap();
    assert_eq!(remove_partials(&inbox), 2);
    assert!(!idle.exists() && !note.exists() && busy.exists());
    drop(writing);
    assert_eq!(remove_partials(&inbox), 1);
    assert!(!inbox.join(PARTIAL_DIR).exists());
}
