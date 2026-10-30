//! T094: CPU while a phone streams live stats (target: under 1%). The agent and the phone run
//! in this one process, so the number is an upper bound for the agent alone. Idle CPU and RAM
//! are measured on the real binary; see specs/001-v1-launch/quickstart.md.
//!
//! cargo test --release -p pocketgpu --test footprint -- --ignored --nocapture
#![cfg(unix)]

mod harness;

use std::time::{Duration, Instant};

use harness::Harness;
use pocketgpu_proto::Permission;

/// User and system CPU time used by this process so far.
fn cpu_time() -> Duration {
    // SAFETY: an all-zero rusage is valid, and getrusage only writes into it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: RUSAGE_SELF with a valid pointer has no other preconditions.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let time = |tv: libc::timeval| {
        Duration::from_secs(tv.tv_sec as u64) + Duration::from_micros(tv.tv_usec as u64)
    };
    time(usage.ru_utime) + time(usage.ru_stime)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "takes a minute; run in release to check the T094 target"]
async fn streaming_stays_under_one_percent() {
    let h = Harness::start().await;
    // The daemon's once-a-second sampler; nothing listens on the Ollama address.
    let sampler = h.agent.sampler.clone();
    tokio::spawn(sampler.run(h.agent.db.clone(), "http://127.0.0.1:9".into()));
    let phone = h.phone(Some(Permission::View)).await;
    let machine = h.connect(&phone).await.unwrap();
    let mut stream = machine.subscribe(Duration::from_secs(1)).await.unwrap();
    let next = async |stream: &mut pocketgpu_client::MetricsStream| {
        tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("a sample every second")
            .unwrap()
            .expect("the stream ended")
    };
    for _ in 0..3 {
        next(&mut stream).await;
    }

    let (cpu, start) = (cpu_time(), Instant::now());
    let mut samples = 0;
    while start.elapsed() < Duration::from_secs(60) {
        next(&mut stream).await;
        samples += 1;
    }
    let wall = start.elapsed();
    let percent = (cpu_time() - cpu).as_secs_f64() / wall.as_secs_f64() * 100.0;
    println!(
        "streaming: {percent:.2}% CPU over {:.0} s, {samples} samples (agent and phone together)",
        wall.as_secs_f64()
    );
    assert!(
        samples >= 55,
        "a sample should arrive every second, got {samples}"
    );
    assert!(
        percent < 1.0,
        "{percent:.2}% CPU while streaming; the target is under 1%"
    );
}
