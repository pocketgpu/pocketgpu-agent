//! A phone reads GPU history over `pgpu/control/1`.

mod harness;

use harness::Harness;
use pocketgpu_proto::Permission;
use pocketgpu_proto::msg::control::{Range, Request, Response};

#[tokio::test]
async fn phones_read_history() {
    let h = Harness::start().await;
    let now = pocketgpu::store::db::now_ms();
    {
        let db = h.agent.db();
        for m in 0..120 {
            db.execute(
                "INSERT INTO metrics_1m (at, gpu, util_min, util_avg, util_max, vram_max_mb, temp_max_c)
                 VALUES (?1, 0, 40, 50, 60, 8000, 70)",
                [now - m * 60_000],
            )
            .unwrap();
        }
    }
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    let start = std::time::Instant::now();
    let reply = machine
        .control(Request::GetHistory {
            range: Range::H24,
            gpu: 0,
        })
        .await
        .unwrap();
    let Response::History(points) = reply else {
        panic!("expected History, got {reply:?}");
    };
    // F05: a 24-hour query returns in under 300 ms on a direct connection.
    assert!(
        start.elapsed().as_millis() < 300,
        "took {:?}",
        start.elapsed()
    );
    assert!(!points.is_empty() && points.len() <= 241);
    assert!(
        points
            .iter()
            .all(|p| p.util_max == 60 && p.temp_max_c == 70)
    );
}

#[tokio::test]
async fn phones_read_the_last_ten_minutes_without_disks() {
    let h = Harness::start().await;
    for _ in 0..3 {
        h.agent.sampler.take(None);
    }
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    let reply = machine.control(Request::GetRecent).await.unwrap();
    let Response::Recent(samples) = reply else {
        panic!("expected Recent, got {reply:?}");
    };
    assert!(samples.len() >= 3);
    assert!(samples.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
    assert!(samples.iter().all(|s| s.disks.is_empty()));
}
