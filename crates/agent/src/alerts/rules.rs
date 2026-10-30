//! Rule alerts (T071, Pro): GPU temperature above a limit for a minute, a disk above a
//! limit, and Ollama down. Each rule fires at most once every 15 minutes per target.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use pocketgpu_proto::Sample;
use pocketgpu_proto::push::{AlertKind, AlertRequest};
use rusqlite::{Connection, OptionalExtension, params};

use super::client;
use crate::daemon::state::Agent;
use crate::store::config::AlertConfig;
use crate::store::devices;

/// How long a GPU must stay hot, or Ollama down, before it counts.
pub const HOLD_MS: u64 = 60_000;
/// The same alert at most this often per rule and target (FR-022).
pub const REPEAT_AFTER_MS: i64 = 15 * 60_000;

/// An alert the rules decided to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Fired {
    pub kind: AlertKind,
    /// What it's about, for de-duplication: `gpu0`, a mount point, or `ollama`.
    pub target: String,
    pub title: String,
    pub body: String,
}

/// What the rules remember between samples.
#[derive(Debug, Default)]
pub struct Rules {
    hot_since: HashMap<u32, u64>,
    ollama_seen_up: bool,
    ollama_down_since: Option<u64>,
}

impl Rules {
    /// Looks at one sample; returns the alerts that are due (before de-duplication).
    pub fn observe(
        &mut self,
        s: &Sample,
        cfg: &AlertConfig,
        machine: &str,
        generic: bool,
    ) -> Vec<Fired> {
        let mut out = Vec::new();
        let named = |specific: String, plain: &str| {
            if generic {
                format!("{plain} on {machine}")
            } else {
                format!("{specific} · {machine}")
            }
        };

        if let Some(limit) = cfg.gpu_temp_c {
            for g in &s.gpus {
                let hot = g.temp_c.is_some_and(|t| f32::from(t) >= limit);
                if !hot {
                    self.hot_since.remove(&g.index);
                    continue;
                }
                let since = *self.hot_since.entry(g.index).or_insert(s.at_ms);
                if s.at_ms.saturating_sub(since) >= HOLD_MS {
                    out.push(Fired {
                        kind: AlertKind::GpuTemp,
                        target: format!("gpu{}", g.index),
                        title: named(
                            format!("GPU {} at {}°C", g.index, g.temp_c.unwrap_or(0)),
                            "A GPU is running hot",
                        ),
                        body: format!("Above your {limit:.0}°C limit for a minute"),
                    });
                }
            }
        } else {
            self.hot_since.clear();
        }

        if let Some(limit) = cfg.disk_full_pct {
            for d in &s.disks {
                if d.total_gb <= 0.0 {
                    continue;
                }
                let pct = d.used_gb / d.total_gb * 100.0;
                if pct >= limit {
                    out.push(Fired {
                        kind: AlertKind::DiskFull,
                        target: d.mount.clone(),
                        title: named(
                            // Mount paths can be chosen by other users (FUSE): cleaned.
                            format!("Disk {} at {pct:.0}%", crate::audit::clean(&d.mount)),
                            "A disk is nearly full",
                        ),
                        body: format!("Above your {limit:.0}% limit"),
                    });
                }
            }
        }

        match s.ollama_up {
            Some(true) => {
                self.ollama_seen_up = true;
                self.ollama_down_since = None;
            }
            // Only alert for an Ollama that was running: a machine without it stays quiet.
            Some(false) if cfg.ollama_down && self.ollama_seen_up => {
                let since = *self.ollama_down_since.get_or_insert(s.at_ms);
                if s.at_ms.saturating_sub(since) >= HOLD_MS {
                    out.push(Fired {
                        kind: AlertKind::OllamaDown,
                        target: "ollama".into(),
                        title: named("Ollama is down".into(), "Ollama is down"),
                        body: "It stopped answering a minute ago".into(),
                    });
                }
            }
            _ => {}
        }
        out
    }
}

/// Whether `rule`/`target` may fire at `now_ms`; if so, records it as fired.
pub fn claim(
    db: &Connection,
    kind: AlertKind,
    target: &str,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let rule = format!("{kind:?}");
    let last: Option<i64> = db
        .query_row(
            "SELECT last_fired_at FROM alert_state WHERE rule = ?1 AND target = ?2",
            params![rule, target],
            |r| r.get(0),
        )
        .optional()?;
    if last.is_some_and(|t| now_ms - t < REPEAT_AFTER_MS) {
        return Ok(false);
    }
    db.execute(
        "INSERT INTO alert_state (rule, target, last_fired_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(rule, target) DO UPDATE SET last_fired_at = ?3",
        params![rule, target, now_ms],
    )?;
    Ok(true)
}

/// Runs until the agent stops: checks every sample and sends what's due.
pub async fn run(agent: Arc<Agent>) {
    let Ok(http) = client::http() else { return };
    let mut samples = agent.sampler.subscribe();
    let mut rules = Rules::default();
    while samples.changed().await.is_ok() {
        let Some(sample) = samples.borrow_and_update().clone() else {
            continue;
        };
        let (cfg, generic, url) = {
            let c = agent.config.read().expect("config lock poisoned");
            (c.alerts.clone(), c.alerts.generic_text, c.push_url.clone())
        };
        for fired in rules.observe(&sample, &cfg, &agent.name(), generic) {
            // Held alerts aren't claimed, so a problem that outlasts the pause still alerts.
            if super::paused_until(&agent).is_some() {
                continue;
            }
            let now = crate::store::db::now_ms();
            if !claim(&agent.db(), fired.kind, &fired.target, now).unwrap_or(false) {
                continue;
            }
            let channel_alert = super::channels::Alert {
                title: fired.title.clone(),
                body: fired.body.clone(),
                kind: kind_name(fired.kind),
                machine: agent.name(),
                job: None,
            };
            super::channels::send_all(&agent, &channel_alert).await;
            let tickets = devices::push_tickets(&agent.db()).unwrap_or_default();
            if tickets.is_empty() {
                continue;
            }
            let alert = AlertRequest {
                tickets,
                kind: fired.kind,
                title: fired.title,
                body: fired.body,
                link: Some(format!("pocketgpu://machine/{}", agent.identity.agent_id())),
            };
            let retries = [Duration::from_secs(2), Duration::from_secs(10)];
            let sent = match serde_json::to_vec(&alert) {
                Ok(body) => {
                    client::signed_post(&agent, &http, &url, "/v1/alerts", body, &retries).await
                }
                Err(e) => Err(e.into()),
            };
            match sent {
                Ok(_) => {
                    tracing::info!(kind = ?fired.kind, target = %fired.target, "rule alert sent")
                }
                Err(e) => tracing::warn!(kind = ?fired.kind, "rule alert not sent: {e:#}"),
            }
        }
    }
}

fn kind_name(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::JobEnd => "job_end",
        AlertKind::GpuTemp => "gpu_temp",
        AlertKind::Offline => "offline",
        AlertKind::OllamaDown => "ollama_down",
        AlertKind::DiskFull => "disk_full",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketgpu_proto::{DiskSample, GpuSample};

    fn sample(at_ms: u64, temp: u8, disk_used: f32, ollama: Option<bool>) -> Sample {
        Sample {
            at_ms,
            gpus: vec![GpuSample {
                index: 0,
                util_pct: Some(90),
                vram_used_mb: Some(1),
                temp_c: Some(temp),
                power_w: None,
                fan_pct: None,
            }],
            cpu_pct: 0.0,
            ram_used_mb: 0,
            ram_total_mb: 1,
            disks: vec![DiskSample {
                mount: "/".into(),
                used_gb: disk_used,
                total_gb: 100.0,
            }],
            ollama_up: ollama,
        }
    }

    fn cfg() -> AlertConfig {
        AlertConfig {
            ollama_down: true,
            ..AlertConfig::default()
        }
    }

    #[test]
    fn hot_gpu_fires_after_a_minute() {
        let mut r = Rules::default();
        let c = cfg();
        assert!(
            r.observe(&sample(0, 90, 10.0, None), &c, "rig-1", false)
                .is_empty()
        );
        assert!(
            r.observe(&sample(59_000, 90, 10.0, None), &c, "rig-1", false)
                .is_empty()
        );
        let fired = r.observe(&sample(60_000, 91, 10.0, None), &c, "rig-1", false);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].title, "GPU 0 at 91°C · rig-1");
        // Cooling down resets the minute.
        assert!(
            r.observe(&sample(61_000, 70, 10.0, None), &c, "rig-1", false)
                .is_empty()
        );
        assert!(
            r.observe(&sample(62_000, 90, 10.0, None), &c, "rig-1", false)
                .is_empty()
        );
    }

    #[test]
    fn full_disk_fires_and_generic_text_hides_details() {
        let mut r = Rules::default();
        let fired = r.observe(&sample(0, 40, 95.0, None), &cfg(), "rig-1", true);
        assert_eq!(fired[0].kind, AlertKind::DiskFull);
        assert_eq!(fired[0].title, "A disk is nearly full on rig-1");
        let off = AlertConfig {
            disk_full_pct: None,
            ..cfg()
        };
        assert!(
            r.observe(&sample(0, 40, 95.0, None), &off, "rig-1", false)
                .is_empty()
        );
    }

    #[test]
    fn ollama_down_needs_it_to_have_run() {
        let mut r = Rules::default();
        let c = cfg();
        assert!(
            r.observe(&sample(0, 40, 1.0, Some(false)), &c, "m", false)
                .is_empty()
        );
        assert!(
            r.observe(&sample(90_000, 40, 1.0, Some(false)), &c, "m", false)
                .is_empty()
        );
        r.observe(&sample(100_000, 40, 1.0, Some(true)), &c, "m", false);
        r.observe(&sample(101_000, 40, 1.0, Some(false)), &c, "m", false);
        let fired = r.observe(&sample(161_000, 40, 1.0, Some(false)), &c, "m", false);
        assert_eq!(fired[0].kind, AlertKind::OllamaDown);
    }

    #[test]
    fn the_same_alert_waits_fifteen_minutes() {
        let db = crate::store::db::open_in_memory().unwrap();
        assert!(claim(&db, AlertKind::GpuTemp, "gpu0", 0).unwrap());
        assert!(!claim(&db, AlertKind::GpuTemp, "gpu0", 60_000).unwrap());
        assert!(claim(&db, AlertKind::GpuTemp, "gpu1", 60_000).unwrap());
        assert!(claim(&db, AlertKind::GpuTemp, "gpu0", REPEAT_AFTER_MS).unwrap());
    }
}
