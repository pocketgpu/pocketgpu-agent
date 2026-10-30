//! NVIDIA GPUs through NVML. The library is loaded at runtime, so the agent starts without it.

use std::sync::Mutex;

use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::enums::device::UsedGpuMemory;
use pocketgpu_proto::{GpuInfo, GpuSample};

use super::{GpuCollector, GpuProcess};

pub struct NvmlGpus {
    nvml: Option<Mutex<Nvml>>,
    error: Option<String>,
}

/// On Windows, NVML only from where the driver installs it: loading `nvml.dll` by name
/// would also search the current folder and every PATH folder.
#[cfg(windows)]
fn load() -> Result<Nvml, nvml_wrapper::error::NvmlError> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let candidates = [
        std::path::Path::new(&root).join(r"System32\nvml.dll"),
        std::path::PathBuf::from(r"C:\Program Files\NVIDIA Corporation\NVSMI\nvml.dll"),
    ];
    let found = candidates
        .iter()
        .find(|p| p.is_file())
        .unwrap_or(&candidates[0]);
    Nvml::builder().lib_path(found.as_os_str()).init()
}

#[cfg(not(windows))]
fn load() -> Result<Nvml, nvml_wrapper::error::NvmlError> {
    Nvml::init()
}

impl NvmlGpus {
    pub fn init() -> Self {
        match load() {
            Ok(nvml) => Self {
                nvml: Some(Mutex::new(nvml)),
                error: None,
            },
            Err(e) => {
                tracing::warn!("NVIDIA GPU stats unavailable: {e}");
                Self {
                    nvml: None,
                    error: Some(match e {
                        nvml_wrapper::error::NvmlError::LibloadingError(_) => {
                            "no NVIDIA driver found. Install it for GPU stats; without it you still get CPU, RAM and disk".into()
                        }
                        e => format!("could not load the NVIDIA driver library: {e}"),
                    }),
                }
            }
        }
    }

    fn with<T>(&self, f: impl FnOnce(&Nvml) -> T) -> Option<T> {
        let nvml = self.nvml.as_ref()?.lock().ok()?;
        Some(f(&nvml))
    }
}

const MB: u64 = 1024 * 1024;

impl GpuCollector for NvmlGpus {
    fn info(&self) -> Vec<GpuInfo> {
        self.with(|nvml| {
            let driver = nvml.sys_driver_version().ok();
            let count = nvml.device_count().unwrap_or(0);
            (0..count)
                .filter_map(|i| {
                    let d = nvml.device_by_index(i).ok()?;
                    Some(GpuInfo {
                        index: i,
                        name: d.name().unwrap_or_else(|_| format!("GPU {i}")),
                        vram_total_mb: d.memory_info().map(|m| m.total / MB).unwrap_or(0),
                        driver: driver.clone(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn sample(&self) -> Vec<GpuSample> {
        self.with(|nvml| {
            let count = nvml.device_count().unwrap_or(0);
            (0..count)
                .filter_map(|i| {
                    let d = nvml.device_by_index(i).ok()?;
                    Some(GpuSample {
                        index: i,
                        util_pct: d.utilization_rates().ok().map(|u| u.gpu.min(100) as u8),
                        vram_used_mb: d.memory_info().ok().map(|m| m.used / MB),
                        temp_c: d
                            .temperature(TemperatureSensor::Gpu)
                            .ok()
                            .map(|t| t.min(255) as u8),
                        power_w: d.power_usage().ok().map(|mw| mw / 1000),
                        fan_pct: d.fan_speed(0).ok().map(|f| f.min(100) as u8),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn processes(&self) -> Vec<GpuProcess> {
        self.with(|nvml| {
            let count = nvml.device_count().unwrap_or(0);
            let mut out = Vec::new();
            for i in 0..count {
                let Ok(d) = nvml.device_by_index(i) else {
                    continue;
                };
                let compute = d.running_compute_processes().unwrap_or_default();
                let graphics = d.running_graphics_processes().unwrap_or_default();
                for p in compute.into_iter().chain(graphics) {
                    if out
                        .iter()
                        .any(|o: &GpuProcess| o.pid == p.pid && o.gpu == i)
                    {
                        continue;
                    }
                    let vram_mb = match p.used_gpu_memory {
                        UsedGpuMemory::Used(bytes) => Some(bytes / MB),
                        UsedGpuMemory::Unavailable => None,
                    };
                    out.push(GpuProcess {
                        pid: p.pid,
                        gpu: i,
                        vram_mb,
                        label: None,
                    });
                }
            }
            out
        })
        .unwrap_or_default()
    }

    fn problem(&self) -> Option<String> {
        self.error.clone()
    }
}
