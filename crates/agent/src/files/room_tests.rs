use super::*;

fn partials() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("pgpu-room-")
        .tempdir()
        .unwrap();
    let partials = dir.path().join(".partial");
    std::fs::create_dir_all(&partials).unwrap();
    (dir, partials)
}

fn partial(dir: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(dir.join(name), bytes).unwrap();
}

#[test]
fn each_device_has_a_few_partials_at_most() {
    let (_tmp, dir) = partials();
    let none = HashMap::new();
    for i in 0..MAX_PARTIALS {
        partial(&dir, &format!("dev1-{i}-1.part"), b"x");
    }
    partial(&dir, "dev2-0-1.part", b"x");
    // A ninth from dev1 is refused; carrying on one of its eight is fine.
    let full = partial_room(&dir, "dev1-", "dev1-new-1.part", 1, &none).unwrap_err();
    assert!(full.message.contains("8 unfinished"), "{}", full.message);
    assert!(partial_room(&dir, "dev1-", "dev1-3-1.part", 1, &none).is_ok());
    assert!(partial_room(&dir, "dev2-", "dev2-1-1.part", 1, &none).is_ok());
    // 64 GB in all, this one's whole size counted.
    let big = partial_room(&dir, "dev2-", "dev2-1-1.part", MAX_PARTIAL_BYTES, &none).unwrap_err();
    assert!(big.message.contains("64 GB"), "{}", big.message);
    // Sends that just started count with the size they started with, not what they hold.
    let running = HashMap::from([("dev2-0-1.part".to_string(), MAX_PARTIAL_BYTES - 5)]);
    let both = partial_room(&dir, "dev2-", "dev2-1-9.part", 9, &running).unwrap_err();
    assert!(
        both.message.contains("finish one first"),
        "{}",
        both.message
    );
    assert!(partial_room(&dir, "dev2-", "dev2-1-5.part", 5, &running).is_ok());
}

#[test]
fn a_file_over_64_gb_is_refused_as_too_big() {
    let (_tmp, dir) = partials();
    let e = partial_room(
        &dir,
        "d-",
        "d-1.part",
        MAX_PARTIAL_BYTES + 1,
        &HashMap::new(),
    );
    let e = e.unwrap_err();
    assert!(e.message.starts_with("files over 64 GB"), "{}", e.message);
    assert!(!e.message.contains("finish one first"), "{}", e.message);
}

#[test]
fn a_send_leaves_a_gigabyte_free() {
    assert!(disk_room(None, u64::MAX, 0).is_ok());
    assert!(disk_room(Some(MIN_FREE + 10), 10, 0).is_ok());
    let e = disk_room(Some(MIN_FREE + 10), 11, 0).unwrap_err();
    assert!(e.message.starts_with("not enough space"), "{}", e.message);
    assert!(disk_room(Some(5), u64::MAX, 0).is_err());
    // Room promised to other sends running now is not free.
    let e = disk_room(Some(MIN_FREE + 10), 10, 1).unwrap_err();
    assert!(e.message.contains("for other sends"), "{}", e.message);
    let (_tmp, dir) = partials();
    assert!(free_space(&dir).is_some_and(|f| f > 0));
    assert!(still_room(Some(MIN_FREE + RECHECK)).is_ok());
    let e = still_room(Some(MIN_FREE + RECHECK - 1)).unwrap_err();
    assert!(e.message.contains("carries on"), "{}", e.message);
    assert!(still_room(None).is_ok());
}

#[test]
fn sends_at_the_same_time_cannot_share_the_same_room() {
    let (_tmp, dir) = partials();
    let gb = 1u64 << 30;
    let free = Some(MIN_FREE + 100 * gb);
    // Four sends of 60 GB each, as if they all started at once with 100 GB free.
    let first = reserve(&dir, "same-", "same-1-x.part", 60 * gb, 0, free).unwrap();
    let second = reserve(&dir, "same-", "same-2-x.part", 60 * gb, 0, free).unwrap_err();
    assert!(second.message.contains("other sends"), "{}", second.message);
    // What it wrote since is on the disk now, so it holds only the rest.
    first.wrote(30 * gb);
    let free_now = Some(MIN_FREE + 70 * gb);
    let third = reserve(&dir, "other-", "other-1-x.part", 40 * gb, 0, free_now).unwrap();
    // The device's own 64 GB counts the size a running send started with.
    let mine = reserve(&dir, "same-", "same-3-x.part", 5 * gb, 0, Some(u64::MAX));
    assert!(mine.unwrap_err().message.contains("64 GB"));
    drop(first);
    let again = reserve(&dir, "same-", "same-3-x.part", 5 * gb, 0, Some(u64::MAX));
    assert!(again.is_ok());
    drop(third);
}
