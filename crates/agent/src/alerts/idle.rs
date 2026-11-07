//! The idle GPU alert (F38, Pro): every GPU under 5% load for the time the user chose.
//! One alert per idle stretch; a stretch starts again once a GPU is busy.

use pocketgpu_proto::Sample;
use pocketgpu_proto::push::AlertKind;

use super::rules::Fired;
use crate::store::config::AlertConfig;

/// A GPU under this load counts as idle.
pub const IDLE_PCT: u8 = 5;

/// What the idle rule remembers between samples.
#[derive(Debug, Default)]
pub struct Idle {
    since: Option<u64>,
    /// Sent for the current idle stretch: once until a GPU is busy again.
    sent: bool,
}

impl Idle {
    /// Looks at one sample; returns the alert once every GPU has been idle long enough.
    /// `named` makes the title, so generic text applies.
    pub fn observe(
        &mut self,
        s: &Sample,
        cfg: &AlertConfig,
        named: impl Fn(String, &str) -> String,
    ) -> Option<Fired> {
        // Every GPU idle, with a load reading for each: a machine without GPUs, or a GPU the
        // driver can't read, never counts as idle.
        let idle = !s.gpus.is_empty()
            && s.gpus
                .iter()
                .all(|g| g.util_pct.is_some_and(|u| u < IDLE_PCT));
        let (Some(min), true) = (cfg.idle_gpu_min, idle) else {
            *self = Self::default();
            return None;
        };
        let since = *self.since.get_or_insert(s.at_ms);
        if self.sent || s.at_ms.saturating_sub(since) < u64::from(min) * 60_000 {
            return None;
        }
        self.sent = true;
        let what = match s.gpus.len() {
            1 => "GPU idle".to_string(),
            n => format!("All {n} GPUs idle"),
        };
        Some(Fired {
            kind: AlertKind::IdleGpu,
            target: "gpus".into(),
            title: named(what, "GPUs are idle"),
            body: body(min, cfg.cents_per_hour),
        })
    }

    /// The alert wasn't sent (alerts were held, or one went out just before): it fires
    /// again on the next idle sample.
    pub fn unsent(&mut self) {
        self.sent = false;
    }
}

/// "Under 5% load for 30 min · $1.90/h".
fn body(min: u32, cents_per_hour: Option<u32>) -> String {
    let time = if min >= 60 && min.is_multiple_of(60) {
        format!("{} h", min / 60)
    } else {
        format!("{min} min")
    };
    match cents_per_hour {
        Some(c) => format!(
            "Under {IDLE_PCT}% load for {time} · ${}.{:02}/h",
            c / 100,
            c % 100
        ),
        None => format!("Under {IDLE_PCT}% load for {time}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::rules::Rules;
    use super::*;
    use pocketgpu_proto::GpuSample;

    const MIN: u64 = 60_000;

    fn loads(at_ms: u64, loads: &[Option<u8>]) -> Sample {
        Sample {
            at_ms,
            gpus: loads
                .iter()
                .enumerate()
                .map(|(i, &u)| GpuSample {
                    index: i as u32,
                    util_pct: u,
                    vram_used_mb: Some(1),
                    temp_c: Some(40),
                    power_w: None,
                    fan_pct: None,
                })
                .collect(),
            cpu_pct: 0.0,
            ram_used_mb: 0,
            ram_total_mb: 1,
            disks: Vec::new(),
            ollama_up: None,
        }
    }

    fn cfg(min: u32, cents: Option<u32>) -> AlertConfig {
        AlertConfig {
            idle_gpu_min: Some(min),
            cents_per_hour: cents,
            ..AlertConfig::default()
        }
    }

    #[test]
    fn idle_gpus_fire_once_per_idle_stretch() {
        let mut r = Rules::default();
        let c = cfg(30, Some(190));
        let quiet = |r: &mut Rules, at: u64, l: &[Option<u8>]| {
            assert!(r.observe(&loads(at, l), &c, "rig-1", false).is_empty());
        };
        quiet(&mut r, 0, &[Some(2), Some(0)]);
        quiet(&mut r, 29 * MIN, &[Some(1), Some(4)]);
        let fired = r.observe(&loads(30 * MIN, &[Some(0), Some(0)]), &c, "rig-1", false);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].kind, AlertKind::IdleGpu);
        assert_eq!(fired[0].title, "All 2 GPUs idle · rig-1");
        assert_eq!(fired[0].body, "Under 5% load for 30 min · $1.90/h");
        // Still idle: nothing more until a GPU is busy again.
        quiet(&mut r, 90 * MIN, &[Some(0), Some(0)]);
        quiet(&mut r, 91 * MIN, &[Some(0), Some(80)]);
        quiet(&mut r, 92 * MIN, &[Some(0), Some(0)]);
        let again = r.observe(&loads(122 * MIN, &[Some(0), Some(0)]), &c, "rig-1", false);
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn an_alert_that_was_not_sent_fires_again() {
        let mut r = Rules::default();
        let c = cfg(10, None);
        r.observe(&loads(0, &[Some(0)]), &c, "m", false);
        let held = r.observe(&loads(10 * MIN, &[Some(0)]), &c, "m", false);
        assert_eq!(held.len(), 1);
        // Alerts were paused: once they resume, the same stretch still alerts.
        r.unsent(&held[0]);
        let later = r.observe(&loads(40 * MIN, &[Some(0)]), &c, "m", false);
        assert_eq!(later, held);
        assert!(
            r.observe(&loads(41 * MIN, &[Some(0)]), &c, "m", false)
                .is_empty()
        );
    }

    #[test]
    fn idle_needs_a_reading_for_every_gpu_and_the_rule_on() {
        let mut r = Rules::default();
        let c = cfg(10, None);
        let late = 11 * MIN;
        // A GPU without a load reading never counts as idle.
        r.observe(&loads(0, &[Some(0), None]), &c, "m", false);
        assert!(
            r.observe(&loads(late, &[Some(0), None]), &c, "m", false)
                .is_empty()
        );
        // No GPUs: a CPU-only machine is never idle in this sense.
        assert!(r.observe(&loads(0, &[]), &c, "m", false).is_empty());
        assert!(r.observe(&loads(late, &[]), &c, "m", false).is_empty());
        // Off.
        let off = AlertConfig {
            idle_gpu_min: None,
            ..c.clone()
        };
        r.observe(&loads(0, &[Some(0)]), &off, "m", false);
        assert!(
            r.observe(&loads(late, &[Some(0)]), &off, "m", false)
                .is_empty()
        );
        // One GPU, hours, no price, generic text.
        let mut r = Rules::default();
        let hours = cfg(120, None);
        r.observe(&loads(0, &[Some(3)]), &hours, "m", true);
        let fired = r.observe(&loads(120 * MIN, &[Some(3)]), &hours, "m", true);
        assert_eq!(fired[0].title, "GPUs are idle on m");
        assert_eq!(fired[0].body, "Under 5% load for 2 h");
    }
}
