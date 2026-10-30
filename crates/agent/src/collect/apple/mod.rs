//! Apple silicon GPU stats for the macOS agent (F26, spike S6), all readable without root:
//!
//! - load and memory in use: the GPU's `PerformanceStatistics` in the IO registry;
//! - power: IOReport energy counters ([`power`]);
//! - temperature: the IOHID sensors ([`temp`]);
//! - processes: the GPU's user clients, which record each app's GPU time.
//!
//! Memory is unified, so the "VRAM" total is the Mac's memory. Fan speed isn't reported.

mod cf;
mod power;
mod temp;

use std::collections::HashMap;
use std::sync::Mutex;

use pocketgpu_proto::{GpuInfo, GpuSample};

use super::{GpuCollector, GpuProcess};
use cf::{Entry, get, items, number, services, string};

const MB: u64 = 1024 * 1024;

pub struct AppleGpus {
    info: Vec<GpuInfo>,
    problem: Option<String>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    power: Option<power::Power>,
    temps: Option<temp::Temps>,
    /// Total GPU time per process (ns) at the last sample.
    gpu_time: HashMap<u32, u64>,
    /// Processes whose GPU time grew since the sample before.
    active: Vec<u32>,
}

fn accelerator() -> Option<Entry> {
    services("IOAccelerator").into_iter().next()
}

impl AppleGpus {
    pub fn init() -> Self {
        let Some(accel) = accelerator() else {
            return Self {
                info: Vec::new(),
                problem: Some(
                    "no Apple GPU found. The macOS agent needs an Apple silicon Mac".into(),
                ),
                state: Mutex::default(),
            };
        };
        let cores = accel.property("gpu-core-count").and_then(|c| number(c.0));
        let name = cf::sysctl_string("machdep.cpu.brand_string")
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Apple GPU".into());
        let info = GpuInfo {
            index: 0,
            name,
            vram_total_mb: cf::sysctl_u64("hw.memsize").unwrap_or(0) / MB,
            driver: cores.map(|c| format!("{c}-core GPU · unified memory")),
        };
        Self {
            info: vec![info],
            problem: None,
            state: Mutex::new(State {
                power: power::Power::new(),
                temps: temp::Temps::new(),
                ..State::default()
            }),
        }
    }
}

/// GPU time per process from the accelerator's user clients: each has
/// `IOUserClientCreator` = "pid 123, Safari" and `AppUsage` entries with `accumulatedGPUTime`.
fn gpu_times(accel: &Entry) -> HashMap<u32, u64> {
    let mut out = HashMap::new();
    for client in accel.children() {
        let Some(pid) = client
            .property("IOUserClientCreator")
            .and_then(|c| string(c.0))
            .and_then(|s| creator_pid(&s))
        else {
            continue;
        };
        let used: i64 = client
            .property("AppUsage")
            .map(|usage| {
                items(usage.0)
                    .into_iter()
                    .filter_map(|u| number(get(u, "accumulatedGPUTime")))
                    .sum()
            })
            .unwrap_or(0);
        *out.entry(pid).or_default() += used.max(0) as u64;
    }
    out
}

fn creator_pid(creator: &str) -> Option<u32> {
    creator
        .strip_prefix("pid ")?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

impl GpuCollector for AppleGpus {
    fn info(&self) -> Vec<GpuInfo> {
        self.info.clone()
    }

    fn sample(&self) -> Vec<GpuSample> {
        if self.info.is_empty() {
            return Vec::new();
        }
        let Some(accel) = accelerator() else {
            return Vec::new();
        };
        let stats = accel.property("PerformanceStatistics");
        let stat = |key: &str| stats.as_ref().and_then(|s| number(get(s.0, key)));
        let mut state = self.state.lock().expect("gpu lock poisoned");
        let watts = state.power.as_mut().and_then(power::Power::gpu_watts);
        let celsius = state.temps.as_ref().and_then(temp::Temps::gpu_celsius);
        let times = gpu_times(&accel);
        let mut active: Vec<u32> = times
            .iter()
            .filter(|(pid, t)| state.gpu_time.get(pid).is_some_and(|before| *t > before))
            .map(|(pid, _)| *pid)
            .collect();
        active.sort_unstable();
        state.active = active;
        state.gpu_time = times;
        vec![GpuSample {
            index: 0,
            util_pct: stat("Device Utilization %").map(|u| u.clamp(0, 100) as u8),
            vram_used_mb: stat("In use system memory").map(|b| b.max(0) as u64 / MB),
            temp_c: celsius.map(|c| c.round() as u8),
            power_w: watts.map(|w| w.round().max(0.0) as u32),
            fan_pct: None,
        }]
    }

    fn processes(&self) -> Vec<GpuProcess> {
        let state = self.state.lock().expect("gpu lock poisoned");
        state
            .active
            .iter()
            .map(|&pid| GpuProcess {
                pid,
                gpu: 0,
                vram_mb: None,
                label: None,
            })
            .collect()
    }

    fn problem(&self) -> Option<String> {
        self.problem.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_pid_of_a_gpu_client() {
        assert_eq!(creator_pid("pid 412, WindowServer"), Some(412));
        assert_eq!(creator_pid("pid 9"), Some(9));
        assert_eq!(creator_pid("kernel"), None);
    }

    /// On a real Mac: one GPU with a name and memory. CI's macOS runners are virtual machines
    /// whose GPU may not report stats, so only the shape is checked.
    #[test]
    fn finds_the_gpu() {
        let gpus = AppleGpus::init();
        if gpus.problem().is_some() {
            return;
        }
        let info = gpus.info();
        assert_eq!(info.len(), 1);
        assert!(info[0].vram_total_mb > 0, "{info:?}");
        let first = gpus.sample();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let second = gpus.sample();
        // Printed so CI logs show what a Mac reports (spike S6).
        println!("{info:?}\n{first:?}\n{second:?}\n{:?}", gpus.processes());
        assert_eq!(second.len(), 1);
    }
}
