use super::*;
use std::io::Write;

#[test]
fn picks_ollamas_own_builds() {
    assert_eq!(
        builds("linux", "x86_64").unwrap(),
        vec![
            Build::TarZst(format!("{BASE}/ollama-linux-amd64.tar.zst")),
            Build::Tgz(format!("{BASE}/ollama-linux-amd64.tgz"))
        ]
    );
    assert_eq!(
        builds("linux", "aarch64").unwrap()[0],
        Build::TarZst(format!("{BASE}/ollama-linux-arm64.tar.zst"))
    );
    assert_eq!(
        builds("macos", "aarch64").unwrap(),
        vec![Build::MacApp(format!("{BASE}/Ollama-darwin.zip"))]
    );
    assert_eq!(
        builds("windows", "x86_64").unwrap(),
        vec![Build::WindowsSetup(format!("{BASE}/OllamaSetup.exe"))]
    );
    assert!(builds("linux", "riscv64").is_err());
    assert!(builds("freebsd", "x86_64").is_err());
}

fn tar_with_server() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let data = b"#!/bin/sh\necho ollama\n";
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o755);
    h.set_cksum();
    b.append_data(&mut h, "bin/ollama", &data[..]).unwrap();
    b.into_inner().unwrap()
}

#[test]
fn unpacks_both_kinds_of_linux_build() {
    let dir = std::env::temp_dir().join(format!("pgpu-setup-{}", std::process::id()));
    let tar = tar_with_server();

    let zst =
        ruzstd::encoding::compress_to_vec(&tar[..], ruzstd::encoding::CompressionLevel::Fastest);
    let home = dir.join("zst");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("o.tar.zst");
    std::fs::write(&file, zst).unwrap();
    install(&Build::TarZst(String::new()), &file, &home).unwrap();
    assert!(home.join("bin/ollama").is_file());

    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    let file = dir.join("o.tgz");
    std::fs::write(&file, gz.finish().unwrap()).unwrap();
    let home = dir.join("tgz");
    let tgz = Build::Tgz(String::new());
    assert_eq!(existing(&tgz, &home), None);
    install(&tgz, &file, &home).unwrap();
    assert!(home.join("bin/ollama").is_file());
    assert_eq!(
        existing(&tgz, &home),
        Some(home.join("bin/ollama")),
        "set up once, then only started"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("bin/ollama"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "the server can run");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_setup_under_way_is_not_started_twice() {
    let agent = Agent::in_memory(Arc::new(crate::collect::fake::FakeGpus::new(1))).unwrap();
    agent.ollama_setup.set(|s| s.phase = SetupPhase::Download);
    let now = start(&agent, "phone");
    assert_eq!(now.phase, SetupPhase::Download);
    assert_eq!(now.done, 0);
    assert!(!agent.ollama_setup.begin(), "under way");
    agent.ollama_setup.set(|s| s.phase = SetupPhase::Failed);
    assert!(agent.ollama_setup.begin(), "tried again after it failed");
}
