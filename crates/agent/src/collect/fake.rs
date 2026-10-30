//! Fake GPUs for development and tests on machines without an NVIDIA card.
//!
//! GPU 0 runs a demo "training job": a real, harmless background process (`sleep`, or `waitfor`
//! on Windows) that the phone lists and can kill. While it runs, GPU 0 is busy and warm; once
//! it's killed the GPU idles and its VRAM frees up, and a new demo job starts a minute later.
//! Temperature, power and fan follow utilisation so the overview moves like a real card.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pocketgpu_proto::{GpuInfo, GpuSample};

use super::{GpuCollector, GpuProcess};

const MODELS: [(&str, u64); 2] = [
    ("NVIDIA GeForce RTX 4090", 24_564),
    ("NVIDIA GeForce RTX 3090", 24_576),
];
/// How long GPU 0 stays idle after its demo job is killed.
const RESPAWN_AFTER: Duration = Duration::from_secs(60);
const DEMO_LABEL: &str = "python train.py --epochs 40";
const DEMO_VRAM_MB: u64 = 14_100;
/// The agent's own share of VRAM (it shows up as a GPU process too, and can't be killed).
const AGENT_VRAM_MB: u64 = 600;

pub struct FakeGpus {
    count: u32,
    tick: AtomicU64,
    demo: Mutex<Demo>,
}

#[derive(Default)]
struct Demo {
    child: Option<std::process::Child>,
    ended_at: Option<Instant>,
}

impl FakeGpus {
    pub fn new(count: u32) -> Self {
        Self {
            count,
            tick: AtomicU64::new(0),
            demo: Mutex::new(Demo::default()),
        }
    }

    /// The demo job's PID, starting it if it isn't running and the idle minute is over.
    fn demo_pid(&self) -> Option<u32> {
        let mut demo = self.demo.lock().ok()?;
        if let Some(child) = demo.child.as_mut()
            && !matches!(child.try_wait(), Ok(None))
        {
            demo.child = None;
            demo.ended_at = Some(Instant::now());
        }
        if demo.child.is_none() && demo.ended_at.is_none_or(|t| t.elapsed() >= RESPAWN_AFTER) {
            demo.child = spawn_demo();
            demo.ended_at = None;
        }
        demo.child.as_ref().map(|c| c.id())
    }
}

impl Drop for FakeGpus {
    fn drop(&mut self) {
        if let Ok(demo) = self.demo.get_mut()
            && let Some(child) = demo.child.as_mut()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(target_os = "linux")]
fn spawn_demo() -> Option<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("infinity")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: prctl is async-signal-safe; the demo dies with the agent instead of lingering.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    cmd.spawn().ok()
}

/// `waitfor` waits for a signal that never comes (up to 27 hours) and uses no CPU. Unlike on
/// Linux, it outlives an agent that crashes; the next agent starts a new one.
#[cfg(windows)]
fn spawn_demo() -> Option<std::process::Child> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("waitfor")
        .args(["/t", "99999", "PocketGPUDemoJob"])
        .creation_flags(0x0800_0000)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn spawn_demo() -> Option<std::process::Child> {
    None
}

impl GpuCollector for FakeGpus {
    fn info(&self) -> Vec<GpuInfo> {
        (0..self.count)
            .map(|i| {
                let (name, vram) = MODELS[i as usize % MODELS.len()];
                GpuInfo {
                    index: i,
                    name: name.into(),
                    vram_total_mb: vram,
                    driver: Some("560.35.03 (fake)".into()),
                }
            })
            .collect()
    }

    fn sample(&self) -> Vec<GpuSample> {
        let t = self.tick.fetch_add(1, Ordering::Relaxed) as f64;
        let training = self.count > 0 && self.demo_pid().is_some();
        (0..self.count)
            .map(|i| {
                let phase = i as f64 * 1.7;
                let wave = (t / 9.0 + phase).sin() * 4.0 + (t / 2.3 + phase).sin() * 2.5;
                let busy = i == 0 && training;
                // A short dip every two minutes, like an evaluation step.
                let util = if !busy {
                    3.0 + (t / 4.0 + phase).sin().abs() * 3.0
                } else if (t as u64) % 120 < 4 {
                    32.0 + wave
                } else {
                    88.0 + wave
                };
                let util = util.clamp(0.0, 100.0);
                let vram = if busy {
                    AGENT_VRAM_MB + DEMO_VRAM_MB + (wave * 45.0) as u64
                } else if i == 0 {
                    AGENT_VRAM_MB
                } else {
                    420
                };
                GpuSample {
                    index: i,
                    util_pct: Some(util as u8),
                    vram_used_mb: Some(vram),
                    temp_c: Some((36.0 + util * 0.4 + (t / 5.0 + phase).sin() * 1.5) as u8),
                    power_w: Some((24.0 + util * 4.0 + wave * 5.0).max(18.0) as u32),
                    fan_pct: Some((28.0 + util * 0.45 + (t / 7.0 + phase).sin() * 2.0) as u8),
                }
            })
            .collect()
    }

    fn processes(&self) -> Vec<GpuProcess> {
        if self.count == 0 {
            return Vec::new();
        }
        let mut out = vec![GpuProcess {
            pid: std::process::id(),
            gpu: 0,
            vram_mb: Some(AGENT_VRAM_MB),
            label: None,
        }];
        if let Some(pid) = self.demo_pid() {
            out.push(GpuProcess {
                pid,
                gpu: 0,
                vram_mb: Some(DEMO_VRAM_MB),
                label: Some(DEMO_LABEL.into()),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_every_gpu_within_bounds() {
        let gpus = FakeGpus::new(2);
        for _ in 0..200 {
            let s = gpus.sample();
            assert_eq!(s.len(), 2);
            assert!(s.iter().all(|g| g.util_pct.unwrap() <= 100));
        }
        assert_eq!(gpus.info()[1].name, "NVIDIA GeForce RTX 3090");
    }

    #[test]
    fn stats_move_with_the_load() {
        let gpus = FakeGpus::new(1);
        let temps: Vec<u8> = (0..30).map(|_| gpus.sample()[0].temp_c.unwrap()).collect();
        assert!(temps.iter().max() > temps.iter().min(), "{temps:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn killing_the_demo_job_frees_the_gpu() {
        let gpus = FakeGpus::new(1);
        let procs = gpus.processes();
        let demo = procs
            .iter()
            .find(|p| p.label.is_some())
            .expect("demo job runs");
        assert!(gpus.sample()[0].vram_used_mb.unwrap() > 10_000);
        // SAFETY: kill has no memory-safety preconditions.
        unsafe { libc::kill(demo.pid as libc::pid_t, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(5);
        while gpus.processes().iter().any(|p| p.label.is_some()) {
            assert!(Instant::now() < deadline, "the demo job should end");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(gpus.sample()[0].vram_used_mb, Some(AGENT_VRAM_MB));
        assert_eq!(
            gpus.processes().len(),
            1,
            "no new demo job within the idle minute"
        );
    }
}
