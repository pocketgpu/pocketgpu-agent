//! What the command line prints. Review changes with `cargo insta review`.

use pocketgpu::cli::doctor::{self, Check};
use pocketgpu::cli::{actions, alerts, audit, jobs, status};
use pocketgpu_proto::local::{AuditEntry, Status};
use pocketgpu_proto::msg::control::Action;
use pocketgpu_proto::msg::jobs::{JobState, JobSummary};
use pocketgpu_proto::{GpuInfo, GpuSample, Sample};

fn status_fixture(paired: u32, connected: u32, direct: u32) -> Status {
    Status {
        name: "rig-1".into(),
        agent_id: "a91c".repeat(16),
        fingerprint: "a91c…a91c".into(),
        version: "0.1.0".into(),
        gpus: vec![GpuInfo {
            index: 0,
            name: "NVIDIA GeForce RTX 4090".into(),
            vram_total_mb: 24564,
            driver: Some("560.35".into()),
        }],
        latest: Some(Sample {
            at_ms: 0,
            gpus: vec![GpuSample {
                index: 0,
                util_pct: Some(87),
                vram_used_mb: Some(18636),
                temp_c: Some(71),
                power_w: Some(380),
                fan_pct: Some(64),
            }],
            cpu_pct: 21.0,
            ram_used_mb: 18000,
            ram_total_mb: 64000,
            disks: vec![],
            ollama_up: Some(true),
        }),
        gpu_problem: None,
        paired_devices: paired,
        connected_devices: connected,
        direct_devices: direct,
        started_ms: 0,
        alerts_paused_until_ms: None,
    }
}

#[test]
fn status_with_a_direct_phone() {
    insta::assert_snapshot!(status::render(&status_fixture(1, 1, 1)));
}

#[test]
fn status_before_pairing() {
    let mut s = status_fixture(0, 0, 0);
    s.gpus.clear();
    s.gpu_problem = Some("the NVIDIA driver is not installed".into());
    s.latest = None;
    insta::assert_snapshot!(status::render(&s));
}

#[test]
fn status_json() {
    insta::assert_json_snapshot!(status::to_json(&status_fixture(2, 2, 1)));
}

#[test]
fn doctor_report() {
    let checks = vec![
        Check {
            check: "User",
            ok: true,
            detail: "not root".into(),
        },
        Check {
            check: "Agent",
            ok: false,
            detail: "not running. Start it with `pgpu service install`".into(),
        },
        Check {
            check: "Network",
            ok: true,
            detail: doctor::classify(true, Some(false), true).1,
        },
    ];
    insta::assert_snapshot!(doctor::render(&checks));
}

#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn systemd_unit() {
    use pocketgpu::cli::service;
    use std::path::Path;
    insta::assert_snapshot!(service::unit_file(Path::new(
        "/home/ana/.local/bin/pocketgpu"
    )));
}

#[test]
fn jobs_table() {
    let job = |id, name: &str, state, started_ms, ended_ms, exit_code| JobSummary {
        id,
        name: name.into(),
        state,
        started_ms,
        ended_ms,
        exit_code,
    };
    let now = 10_000_000;
    let list = vec![
        job(
            14,
            "train.py",
            JobState::Running,
            now - 3_720_000,
            None,
            None,
        ),
        job(
            13,
            "eval.py",
            JobState::Crashed,
            now - 900_000,
            Some(now - 700_000),
            Some(1),
        ),
        job(
            12,
            "next.sh",
            JobState::Finished,
            0,
            Some(8_040_000),
            Some(0),
        ),
    ];
    insta::assert_snapshot!(jobs::render(&list, now));
    insta::assert_snapshot!("jobs_table_empty", jobs::render(&[], now));
}

#[test]
fn alerts_table() {
    let rules =
        pocketgpu::alerts::settings::rules(&pocketgpu::store::config::AlertConfig::default());
    insta::assert_snapshot!(alerts::render(&rules));
}

#[test]
fn actions_table() {
    let list = vec![
        Action {
            id: 1,
            name: "Restart vLLM".into(),
            command: "systemctl --user restart vllm".into(),
            icon: "rotate-ccw".into(),
            risky: true,
            timeout_s: 60,
        },
        Action {
            id: 2,
            name: "Disk usage".into(),
            command: "df -h /data".into(),
            icon: "hard-drive".into(),
            risky: false,
            timeout_s: 600,
        },
    ];
    insta::assert_snapshot!(actions::render(&list));
    insta::assert_snapshot!("actions_table_empty", actions::render(&[]));
}

#[test]
fn audit_table() {
    let now = 10 * 86_400_000;
    let entry = |ago_ms: i64, device: &str, event: &str, target: &str, result: &str| AuditEntry {
        at_ms: now - ago_ms,
        device: device.into(),
        event: event.into(),
        target: target.into(),
        result: result.into(),
    };
    let entries = vec![
        entry(20_000, "Pixel 9", "action_run", "#1 Restart vLLM", "exit 0"),
        entry(5 * 60_000, "Pixel 9", "kill", "pid 48213", "sigterm sent"),
        entry(
            3 * 3_600_000,
            "a91c40de…",
            "refused",
            "CreateAction x",
            "needs shell permission",
        ),
        entry(
            2 * 86_400_000,
            "local",
            "action_create",
            "#1 Restart vLLM",
            "ok",
        ),
    ];
    insta::assert_snapshot!(audit::render(&entries, now));
}

#[test]
fn devices_table() {
    use pocketgpu::cli::devices;
    use pocketgpu_proto::Permission;
    use pocketgpu_proto::local::DeviceInfo;
    let now = 10 * 86_400_000;
    let list = vec![
        DeviceInfo {
            id: "a91c40de".repeat(8),
            name: "Pixel 9".into(),
            platform: "android".into(),
            permission: Permission::Shell,
            paired_at_ms: 0,
            last_seen_ms: Some(now),
            connected: true,
        },
        DeviceInfo {
            id: "7f3a9c1b".repeat(8),
            name: "iPhone".into(),
            platform: "ios".into(),
            permission: Permission::View,
            paired_at_ms: 0,
            last_seen_ms: Some(now - 3 * 3_600_000),
            connected: false,
        },
    ];
    insta::assert_snapshot!(devices::render(&list, now));
    insta::assert_snapshot!("devices_table_empty", devices::render(&[], now));
}

#[test]
fn files_status() {
    use pocketgpu::cli::files;
    use pocketgpu_proto::local::FilesStatus;
    let on = FilesStatus {
        enabled: true,
        shared: vec!["~/ComfyUI/output".into(), "/mnt/data/runs".into()],
        inbox: "~/PocketGPU/Inbox".into(),
        inbox_files: 3,
        offers: 1,
    };
    insta::assert_snapshot!(files::render(&on));
    let off = FilesStatus {
        enabled: false,
        shared: vec![],
        inbox_files: 0,
        offers: 0,
        ..on.clone()
    };
    insta::assert_snapshot!("files_status_off", files::render(&off));
    insta::assert_snapshot!("files_inbox", files::render_inbox(&on, false));
}

#[test]
fn inbox_table() {
    use pocketgpu::cli::inbox;
    use pocketgpu_proto::local::InboxFile;
    let now = 10_000_000;
    let files = vec![
        InboxFile {
            name: "IMG_2041.jpg".into(),
            size: 2_400_000,
            from: "Pixel 8".into(),
            at_ms: now - 5 * 60_000,
        },
        InboxFile {
            name: "dataset-2026-11-09-final-final-v2-cleaned.tar.gz".into(),
            size: 3 << 30,
            from: String::new(),
            at_ms: now - 3 * 3_600_000,
        },
    ];
    insta::assert_snapshot!(inbox::render("~/PocketGPU/Inbox", &files, now));
    insta::assert_snapshot!(
        "inbox_table_empty",
        inbox::render("~/PocketGPU/Inbox", &[], now)
    );
}

#[test]
fn send_offered() {
    use pocketgpu::cli::send::{self, Offered};
    let offered = Offered {
        name: "adapter.safetensors".into(),
        size: 320 << 20,
        to: vec!["Pixel 8".into(), "MacBook".into()],
        expires_ms: 7 * 86_400_000,
        pushed: vec!["Pixel 8".into()],
    };
    insta::assert_snapshot!(send::render(&offered, 0));
    let nobody = Offered {
        to: vec![],
        pushed: vec![],
        ..offered
    };
    insta::assert_snapshot!("send_offered_to_nobody", send::render(&nobody, 0));
}

#[test]
fn clip_status() {
    use pocketgpu::cli::clip;
    use pocketgpu_proto::clip::Place;
    use pocketgpu_proto::local::ClipStatus;
    let memory = ClipStatus {
        enabled: true,
        place: Place::Memory,
        has_text: true,
        clears_in_s: Some(540),
    };
    insta::assert_snapshot!(clip::render(&memory));
    let system = ClipStatus {
        place: Place::System,
        has_text: false,
        clears_in_s: None,
        ..memory.clone()
    };
    insta::assert_snapshot!("clip_status_system", clip::render(&system));
    insta::assert_snapshot!("clip_put_once", clip::render_put(&memory, true));
}
