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
                let rows = summarize(&self.window(minute_start, minute_start + 60_000));
                if let Ok(db) = db.lock()
                    && let Err(e) = write_minute(&db, minute_start as i64, &rows)
                {
                    tracing::warn!("could not save history: {e}");
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
}
