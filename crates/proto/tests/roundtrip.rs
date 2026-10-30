use pocketgpu_proto::msg::{control, jobs, pair};
use pocketgpu_proto::{
    FrameError, GpuSample, MAX_FRAME, Permission, Platform, Sample, frame, read_frame, write_frame,
};

async fn roundtrip<T>(msg: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let mut buf = Vec::new();
    write_frame(&mut buf, &msg).await.unwrap();
    let mut r = buf.as_slice();
    let back: T = read_frame(&mut r).await.unwrap().unwrap();
    assert_eq!(back, msg);
    let end: Option<T> = read_frame(&mut r).await.unwrap();
    assert!(end.is_none(), "stream should end cleanly after one frame");
}

#[tokio::test]
async fn messages_roundtrip() {
    roundtrip(pair::PairRequest {
        secret_proof: [7; 32],
        device_name: "iPhone 16".into(),
        platform: Platform::Ios,
        push_ticket: Some("t1".into()),
    })
    .await;
    roundtrip(pair::Response::Pending {
        agent_id: [3; 32],
        agent_name: "rig-1".into(),
    })
    .await;
    roundtrip(pair::Response::Approved {
        permission: Permission::Control,
        relays: vec!["https://relay.example.com".into()],
    })
    .await;
    roundtrip(control::Request::KillProcess { pid: 48213 }).await;
    roundtrip(control::Response::ActionResult {
        exit_code: Some(0),
        output: "ok".into(),
        duration_ms: 12,
        timed_out: false,
    })
    .await;
    roundtrip(jobs::Request::FollowLog {
        id: 12,
        from_offset: 4096,
    })
    .await;
    roundtrip(Sample {
        at_ms: 1,
        gpus: vec![GpuSample {
            index: 0,
            util_pct: Some(87),
            vram_used_mb: Some(18_200),
            temp_c: Some(71),
            power_w: None,
            fan_pct: None,
        }],
        cpu_pct: 34.0,
        ram_used_mb: 26_000,
        ram_total_mb: 64_000,
        disks: vec![],
        ollama_up: Some(true),
    })
    .await;
}

#[tokio::test]
async fn oversize_frames_are_rejected() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
    let mut r = buf.as_slice();
    let res: Result<Option<control::Request>, _> = read_frame(&mut r).await;
    assert!(matches!(res, Err(FrameError::TooLarge(_))));

    let huge = control::Response::ActionResult {
        exit_code: None,
        output: "x".repeat(MAX_FRAME + 1),
        duration_ms: 0,
        timed_out: false,
    };
    assert!(matches!(frame::encode(&huge), Err(FrameError::TooLarge(_))));
}

#[test]
fn permissions_are_ordered() {
    assert!(Permission::View < Permission::Control);
    assert!(Permission::Control < Permission::Shell);
    assert_eq!(
        control::Request::ListProcesses.min_permission(),
        Permission::Control
    );
    assert_eq!(
        control::Request::DeleteAction { id: 1 }.min_permission(),
        Permission::Shell
    );
    assert_eq!(
        jobs::Request::StopJob { id: 1 }.min_permission(),
        Permission::Control
    );
}
