//! Takes a sample every second, keeps 10 minutes in memory and writes per-minute summaries.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pocketgpu_proto::{GpuSample, Sample};
use rusqlite::{Connection, params};
use tokio::sync::watch;

use super::GpuCollector;
use super::system::SystemCollector;
use crate::store::db::now_ms;

const RING: usize = 600;
pub const KEEP_DAYS: i64 = 30;

pub struct Sampler {
    gpus: Arc<dyn GpuCollector>,
    system: SystemCollector,
    ring: Mutex<VecDeque<Sample>>,
    latest: watch::Sender<Option<Sample>>,
}

impl Sampler {
    pub fn new(gpus: Arc<dyn GpuCollector>) -> Arc<Self> {
        Arc::new(Self {
            gpus,
            system: SystemCollector::new(),
            ring: Mutex::new(VecDeque::with_capacity(RING)),
            latest: watch::channel(None).0,
        })
    }

    pub fn gpus(&self) -> &Arc<dyn GpuCollector> {
        &self.gpus
    }

    /// Receives every new sample; used by metrics streams.
    pub fn subscribe(&self) -> watch::Receiver<Option<Sample>> {
        self.latest.subscribe()
    }

    pub fn latest(&self) -> Option<Sample> {
        self.latest.borrow().clone()
    }

    pub fn take(&self, ollama_up: Option<bool>) -> Sample {
        let sys = self.system.sample();
        let sample = Sample {
            at_ms: now_ms() as u64,
            gpus: self.gpus.sample(),
            cpu_pct: sys.cpu_pct,
            ram_used_mb: sys.ram_used_mb,
            ram_total_mb: sys.ram_total_mb,
            disks: sys.disks,
            ollama_up,
        };
        let mut ring = self.ring.lock().expect("sample ring lock poisoned");
        if ring.len() == RING {
            ring.pop_front();
        }
        ring.push_back(sample.clone());
        drop(ring);
        self.latest.send_replace(Some(sample.clone()));
        sample
    }

    /// Samples whose timestamp falls in `[from_ms, to_ms)`.
    pub fn window(&self, from_ms: u64, to_ms: u64) -> Vec<Sample> {
        let ring = self.ring.lock().expect("sample ring lock poisoned");
        ring.iter()
            .filter(|s| s.at_ms >= from_ms && s.at_ms < to_ms)
            .cloned()
            .collect()
    }

    /// Runs forever: a sample every second, a summary row per GPU each minute, and an
    /// Ollama check every 10 seconds.
    pub async fn run(self: Arc<Self>, db: Arc<Mutex<Connection>>, ollama_addr: String) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut minute_start = now_ms() as u64 / 60_000 * 60_000;
        let mut ollama_up = None;
        let mut ticks: u64 = 0;
        loop {
            tick.tick().await;
            if ticks.is_multiple_of(10) {
                ollama_up = Some(super::ollama::version(&ollama_addr).await.is_some());
            }
            ticks += 1;
            let me = self.clone();
            let sample = tokio::task::spawn_blocking(move || me.take(ollama_up)).await;
            let Ok(sample) = sample else { continue };
            if sample.at_ms >= minute_start + 60_000 {
                let window = self.window(minute_start, minute_start + 60_000);
                let rows = summarize(&window);
                let system = summarize_system(&window);
                if let Ok(db) = db.lock() {
                    if let Err(e) = write_minute(&db, minute_start as i64, &rows) {
                        tracing::warn!("could not save history: {e}");
                    }
                    if let Some(system) = system
                        && let Err(e) = write_system_minute(&db, minute_start as i64, &system)
                    {
                        tracing::warn!("could not save system history: {e}");
                    }
                }
                minute_start = sample.at_ms / 60_000 * 60_000;
            }
        }
    }
}

/// Min, average and max utilization, max VRAM and max temperature per GPU.
#[derive(Debug, Clone, PartialEq)]
pub struct MinuteRow {
    pub gpu: u32,
    pub util_min: u8,
    pub util_avg: u8,
    pub util_max: u8,
    pub vram_max_mb: u64,
    pub temp_max_c: u8,
}

pub fn summarize(samples: &[Sample]) -> Vec<MinuteRow> {
    let mut rows: Vec<MinuteRow> = Vec::new();
    let mut sums: Vec<(u32, u32)> = Vec::new();
    for g in samples.iter().flat_map(|s| s.gpus.iter()) {
        let GpuSample {
            index,
            util_pct,
            vram_used_mb,
            temp_c,
            ..
        } = *g;
        let util = util_pct.unwrap_or(0);
        match rows.iter().position(|r| r.gpu == index) {
            Some(i) => {
                let r = &mut rows[i];
                r.util_min = r.util_min.min(util);
                r.util_max = r.util_max.max(util);
                r.vram_max_mb = r.vram_max_mb.max(vram_used_mb.unwrap_or(0));
                r.temp_max_c = r.temp_max_c.max(temp_c.unwrap_or(0));
                sums[i].0 += util as u32;
                sums[i].1 += 1;
            }
            None => {
                rows.push(MinuteRow {
                    gpu: index,
                    util_min: util,
                    util_avg: util,
                    util_max: util,
                    vram_max_mb: vram_used_mb.unwrap_or(0),
                    temp_max_c: temp_c.unwrap_or(0),
                });
                sums.push((util as u32, 1));
            }
        }
    }
    for (r, (sum, n)) in rows.iter_mut().zip(sums) {
        r.util_avg = (sum / n.max(1)) as u8;
    }
    rows
}

fn write_minute(db: &Connection, at_ms: i64, rows: &[MinuteRow]) -> rusqlite::Result<()> {
    for r in rows {
        db.execute(
            "INSERT OR REPLACE INTO metrics_1m (at, gpu, util_min, util_avg, util_max, vram_max_mb, temp_max_c)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![at_ms, r.gpu, r.util_min, r.util_avg, r.util_max, r.vram_max_mb as i64, r.temp_max_c],
        )?;
    }
    db.execute(
        "DELETE FROM metrics_1m WHERE at < ?1",
        [at_ms - KEEP_DAYS * 24 * 3600 * 1000],
    )?;
    Ok(())
}

/// Min, average and max of CPU, RAM and the main disk, each 0-100.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemMinuteRow {
    pub cpu_min: u8,
    pub cpu_avg: u8,
    pub cpu_max: u8,
    pub ram_min: u8,
    pub ram_avg: u8,
    pub ram_max: u8,
    pub disk_min: u8,
    pub disk_avg: u8,
    pub disk_max: u8,
}

/// The disk to watch: the root mount, or else the largest (board 22's "Disk" tile; the same
/// choice `mainDisk` makes on the phone, so the live tile and this history agree).
fn main_disk_pct(sample: &Sample) -> Option<f32> {
    let d = sample.disks.iter().find(|d| d.mount == "/").or_else(|| {
        sample
            .disks
            .iter()
            .max_by(|a, b| a.total_gb.total_cmp(&b.total_gb))
    })?;
    (d.total_gb > 0.0).then(|| (d.used_gb / d.total_gb * 100.0).clamp(0.0, 100.0))
}

pub fn summarize_system(samples: &[Sample]) -> Option<SystemMinuteRow> {
    if samples.is_empty() {
        return None;
    }
    let cpu: Vec<f32> = samples.iter().map(|s| s.cpu_pct).collect();
    let ram: Vec<f32> = samples
        .iter()
        .map(|s| {
            if s.ram_total_mb > 0 {
                (s.ram_used_mb as f32 / s.ram_total_mb as f32 * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            }
        })
        .collect();
    let disk: Vec<f32> = samples.iter().filter_map(main_disk_pct).collect();
    let agg = |v: &[f32]| -> (u8, u8, u8) {
        if v.is_empty() {
            return (0, 0, 0);
        }
        let min = v.iter().cloned().fold(f32::MAX, f32::min) as u8;
        let max = v.iter().cloned().fold(f32::MIN, f32::max) as u8;
        let avg = (v.iter().sum::<f32>() / v.len() as f32) as u8;
        (min, avg, max)
    };
    let (cpu_min, cpu_avg, cpu_max) = agg(&cpu);
    let (ram_min, ram_avg, ram_max) = agg(&ram);
    let (disk_min, disk_avg, disk_max) = agg(&disk);
    Some(SystemMinuteRow {
        cpu_min,
        cpu_avg,
        cpu_max,
        ram_min,
        ram_avg,
        ram_max,
        disk_min,
        disk_avg,
        disk_max,
    })
}

fn write_system_minute(db: &Connection, at_ms: i64, row: &SystemMinuteRow) -> rusqlite::Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO metrics_system_1m
            (at, cpu_min, cpu_avg, cpu_max, ram_min, ram_avg, ram_max, disk_min, disk_avg, disk_max)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            at_ms,
            row.cpu_min,
            row.cpu_avg,
            row.cpu_max,
            row.ram_min,
            row.ram_avg,
            row.ram_max,
            row.disk_min,
            row.disk_avg,
            row.disk_max
        ],
    )?;
    db.execute(
        "DELETE FROM metrics_system_1m WHERE at < ?1",
        [at_ms - KEEP_DAYS * 24 * 3600 * 1000],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::fake::FakeGpus;

    #[test]
    fn summarizes_per_gpu() {
        let sampler = Sampler::new(Arc::new(FakeGpus::new(2)));
        let samples: Vec<_> = (0..10).map(|_| sampler.take(None)).collect();
        let rows = summarize(&samples);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].util_min <= rows[0].util_avg && rows[0].util_avg <= rows[0].util_max);
        let db = crate::store::db::open_in_memory().unwrap();
        write_minute(&db, now_ms(), &rows).unwrap();
        let n: i64 = db
            .query_row("SELECT count(*) FROM metrics_1m", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    fn sample_with(cpu_pct: f32, ram_pct: f32, disk_pct: f32) -> Sample {
        Sample {
            at_ms: now_ms() as u64,
            gpus: vec![],
            cpu_pct,
            ram_used_mb: (ram_pct * 10.0) as u64,
            ram_total_mb: 1000,
            disks: vec![pocketgpu_proto::DiskSample {
                mount: "/".into(),
                used_gb: disk_pct,
                total_gb: 100.0,
            }],
            ollama_up: None,
        }
    }

    #[test]
    fn summarizes_cpu_ram_and_the_root_disk() {
        let samples = vec![
            sample_with(10.0, 20.0, 30.0),
            sample_with(50.0, 60.0, 70.0),
            sample_with(30.0, 40.0, 50.0),
        ];
        let row = summarize_system(&samples).unwrap();
        assert_eq!((row.cpu_min, row.cpu_avg, row.cpu_max), (10, 30, 50));
        assert_eq!((row.ram_min, row.ram_avg, row.ram_max), (20, 40, 60));
        assert_eq!((row.disk_min, row.disk_avg, row.disk_max), (30, 50, 70));
        let db = crate::store::db::open_in_memory().unwrap();
        write_system_minute(&db, now_ms(), &row).unwrap();
        let n: i64 = db
            .query_row("SELECT count(*) FROM metrics_system_1m", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn no_samples_is_no_row() {
        assert!(summarize_system(&[]).is_none());
    }

    #[test]
    fn picks_the_largest_disk_without_a_root_mount() {
        let mut s = sample_with(0.0, 0.0, 0.0);
        s.disks = vec![
            pocketgpu_proto::DiskSample {
                mount: "D:\\".into(),
                used_gb: 10.0,
                total_gb: 50.0,
            },
            pocketgpu_proto::DiskSample {
                mount: "C:\\".into(),
                used_gb: 80.0,
                total_gb: 100.0,
            },
        ];
        let row = summarize_system(&[s]).unwrap();
        assert_eq!(row.disk_avg, 80);
    }
}
