//! Stats collection: GPUs behind a trait (NVIDIA, Apple silicon or fake), plus the system.

#[cfg(target_os = "macos")]
pub mod apple;
pub mod fake;
pub mod nvml;
pub mod ollama;
pub mod sampler;
pub mod system;

use std::sync::Arc;

use pocketgpu_proto::{GpuInfo, GpuSample};

/// A process using a GPU, as the driver reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuProcess {
    pub pid: u32,
    pub gpu: u32,
    pub vram_mb: Option<u64>,
    /// A name to show instead of the command line (the fake GPU's demo process).
    pub label: Option<String>,
}

pub trait GpuCollector: Send + Sync {
    /// Static facts, read once at start.
    fn info(&self) -> Vec<GpuInfo>;
    /// One reading per GPU. Unsupported fields are `None`.
    fn sample(&self) -> Vec<GpuSample>;
    fn processes(&self) -> Vec<GpuProcess>;
    /// Why GPU stats are missing, for `pgpu doctor`. `None` when everything works.
    fn problem(&self) -> Option<String> {
        None
    }
}

/// The fake collector when `PGPU_FAKE_GPU=1`, the Apple GPU on macOS, otherwise NVML (which
/// may report no GPUs).
pub fn from_env() -> Arc<dyn GpuCollector> {
    if std::env::var("PGPU_FAKE_GPU").is_ok_and(|v| v != "0") {
        tracing::info!("using fake GPUs (PGPU_FAKE_GPU is set)");
        return Arc::new(fake::FakeGpus::new(fake_count()));
    }
    #[cfg(target_os = "macos")]
    return Arc::new(apple::AppleGpus::init());
    #[cfg(not(target_os = "macos"))]
    Arc::new(nvml::NvmlGpus::init())
}

fn fake_count() -> u32 {
    std::env::var("PGPU_FAKE_GPU")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| (1..=8).contains(n))
        .unwrap_or(1)
}
