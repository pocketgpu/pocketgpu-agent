//! CPU, memory and disks.

use std::sync::Mutex;

use pocketgpu_proto::DiskSample;
use sysinfo::{Disks, System};

pub struct SystemStats {
    pub cpu_pct: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub disks: Vec<DiskSample>,
}

pub struct SystemCollector {
    sys: Mutex<System>,
    disks: Mutex<Disks>,
}

const MB: u64 = 1024 * 1024;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

impl Default for SystemCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemCollector {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        Self {
            sys: Mutex::new(sys),
            disks: Mutex::new(Disks::new_with_refreshed_list()),
        }
    }

    pub fn sample(&self) -> SystemStats {
        let (cpu_pct, ram_used_mb, ram_total_mb) = {
            let mut sys = self.sys.lock().expect("system stats lock poisoned");
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            (
                sys.global_cpu_usage(),
                sys.used_memory() / MB,
                sys.total_memory() / MB,
            )
        };
        let disks = {
            let mut disks = self.disks.lock().expect("disk stats lock poisoned");
            disks.refresh(true);
            disks
                .list()
                .iter()
                .filter(|d| d.total_space() > 0 && !d.is_removable())
                .map(|d| DiskSample {
                    mount: d.mount_point().to_string_lossy().into_owned(),
                    used_gb: ((d.total_space() - d.available_space()) as f64 / GB) as f32,
                    total_gb: (d.total_space() as f64 / GB) as f32,
                })
                .collect()
        };
        SystemStats {
            cpu_pct,
            ram_used_mb,
            ram_total_mb,
            disks,
        }
    }
}

pub fn os_name() -> String {
    System::long_os_version().unwrap_or_else(|| std::env::consts::OS.into())
}
