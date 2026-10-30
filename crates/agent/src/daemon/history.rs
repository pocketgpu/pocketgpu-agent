//! `GetHistory`: per-minute GPU summaries (kept 30 days), grouped into at most
//! [`MAX_POINTS`] points so any range is light to send and draw.

use anyhow::Result;
use pocketgpu_proto::msg::control::{MetricMinute, Range};
use rusqlite::{Connection, params};

pub const MAX_POINTS: i64 = 240;
const MINUTE: i64 = 60_000;

pub fn span_ms(range: Range) -> i64 {
    let hour = 60 * MINUTE;
    match range {
        Range::H1 => hour,
        Range::H6 => 6 * hour,
        Range::H24 => 24 * hour,
        Range::D7 => 7 * 24 * hour,
        Range::D30 => 30 * 24 * hour,
    }
}

/// Points for `gpu` over `range` ending at `now_ms`, oldest first. Each point covers one
/// bucket: the lowest minimum, mean average, highest maximum and peaks of its minutes.
pub fn query(db: &Connection, range: Range, gpu: u32, now_ms: i64) -> Result<Vec<MetricMinute>> {
    let span = span_ms(range);
    // Whole minutes, so buckets line up with the stored rows.
    let bucket = (span / MAX_POINTS).max(MINUTE) / MINUTE * MINUTE;
    let mut stmt = db.prepare(
        "SELECT (at / ?1) * ?1 AS t, min(util_min), avg(util_avg), max(util_max),
                max(vram_max_mb), max(temp_max_c)
         FROM metrics_1m WHERE gpu = ?2 AND at >= ?3
         GROUP BY t ORDER BY t",
    )?;
    let rows = stmt.query_map(params![bucket, gpu, now_ms - span], |r| {
        Ok(MetricMinute {
            at_ms: r.get::<_, i64>(0)? as u64,
            gpu,
            util_min: r.get::<_, Option<i64>>(1)?.unwrap_or(0) as u8,
            util_avg: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0).round() as u8,
            util_max: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u8,
            vram_max_mb: r.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
            temp_max_c: r.get::<_, Option<i64>>(5)?.unwrap_or(0) as u8,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::open_in_memory;

    fn insert(db: &Connection, at: i64, gpu: u32, util: u8, temp: u8) {
        db.execute(
            "INSERT INTO metrics_1m (at, gpu, util_min, util_avg, util_max, vram_max_mb, temp_max_c)
             VALUES (?1, ?2, ?3, ?3, ?3, 1000, ?4)",
            params![at, gpu, util, temp],
        )
        .unwrap();
    }

    #[test]
    fn one_hour_is_one_point_per_minute() {
        let db = open_in_memory().unwrap();
        let now = 1_000 * MINUTE;
        for m in 0..90 {
            insert(&db, now - m * MINUTE, 0, (m % 100) as u8, 60);
        }
        insert(&db, now, 1, 99, 90);
        let points = query(&db, Range::H1, 0, now).unwrap();
        assert_eq!(points.len(), 61);
        assert!(points.windows(2).all(|w| w[0].at_ms < w[1].at_ms));
        assert!(points.iter().all(|p| p.gpu == 0 && p.temp_max_c == 60));
    }

    #[test]
    fn long_ranges_are_grouped() {
        let db = open_in_memory().unwrap();
        let now = 100_000 * MINUTE;
        for m in 0..(24 * 60) {
            insert(
                &db,
                now - m * MINUTE,
                0,
                if m % 2 == 0 { 10 } else { 90 },
                50 + (m % 3) as u8,
            );
        }
        let points = query(&db, Range::H24, 0, now).unwrap();
        assert!(
            points.len() <= MAX_POINTS as usize + 1,
            "{} points",
            points.len()
        );
        let p = &points[points.len() / 2];
        assert_eq!((p.util_min, p.util_max, p.util_avg), (10, 90, 50));
        assert_eq!(p.temp_max_c, 52);
    }

    #[test]
    fn empty_history_is_empty() {
        let db = open_in_memory().unwrap();
        assert!(query(&db, Range::D30, 0, 0).unwrap().is_empty());
    }
}
