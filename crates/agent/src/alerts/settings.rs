//! Alert settings (T073): the rule list phones and `pgpu alerts` see, mapped onto
//! `[alerts]` in `config.toml`.

use anyhow::{Result, bail};
use pocketgpu_proto::msg::control::{AlertKind, AlertRule};

use crate::daemon::state::Agent;
use crate::store::config::AlertConfig;

pub const DEFAULT_GPU_TEMP_C: f32 = 85.0;
pub const DEFAULT_DISK_PCT: f32 = 90.0;
pub const DEFAULT_OFFLINE_MIN: f32 = 5.0;

pub fn rules(cfg: &AlertConfig) -> Vec<AlertRule> {
    vec![
        AlertRule {
            kind: AlertKind::JobEnd,
            enabled: cfg.job_end,
            threshold: None,
        },
        AlertRule {
            kind: AlertKind::GpuTemp,
            enabled: cfg.gpu_temp_c.is_some(),
            threshold: Some(cfg.gpu_temp_c.unwrap_or(DEFAULT_GPU_TEMP_C)),
        },
        AlertRule {
            kind: AlertKind::DiskFull,
            enabled: cfg.disk_full_pct.is_some(),
            threshold: Some(cfg.disk_full_pct.unwrap_or(DEFAULT_DISK_PCT)),
        },
        AlertRule {
            kind: AlertKind::Offline,
            enabled: cfg.offline_after_min > 0.0,
            threshold: Some(if cfg.offline_after_min > 0.0 {
                cfg.offline_after_min
            } else {
                DEFAULT_OFFLINE_MIN
            }),
        },
        AlertRule {
            kind: AlertKind::OllamaDown,
            enabled: cfg.ollama_down,
            threshold: None,
        },
        AlertRule {
            kind: AlertKind::GenericText,
            enabled: cfg.generic_text,
            threshold: None,
        },
    ]
}

/// The allowed range for a rule's threshold, and its unit.
pub fn limits(kind: AlertKind) -> Option<(f32, f32, &'static str)> {
    match kind {
        AlertKind::GpuTemp => Some((40.0, 110.0, "°C")),
        AlertKind::DiskFull => Some((50.0, 99.0, "%")),
        AlertKind::Offline => Some((2.0, 24.0 * 60.0, "minutes")),
        _ => None,
    }
}

/// Applies changed rules to the config; nothing changes if any rule is invalid.
pub fn apply(cfg: &AlertConfig, changes: &[AlertRule]) -> Result<AlertConfig> {
    let mut next = cfg.clone();
    for r in changes {
        let value = match (limits(r.kind), r.threshold) {
            (Some((lo, hi, unit)), Some(v)) if !(lo..=hi).contains(&v) => {
                bail!("{:?} must be between {lo} and {hi} {unit}", r.kind)
            }
            (_, v) => v,
        };
        match r.kind {
            AlertKind::JobEnd => next.job_end = r.enabled,
            AlertKind::GpuTemp => {
                next.gpu_temp_c = r
                    .enabled
                    .then(|| value.or(cfg.gpu_temp_c).unwrap_or(DEFAULT_GPU_TEMP_C));
            }
            AlertKind::DiskFull => {
                next.disk_full_pct = r
                    .enabled
                    .then(|| value.or(cfg.disk_full_pct).unwrap_or(DEFAULT_DISK_PCT));
            }
            AlertKind::Offline => {
                let current = (cfg.offline_after_min > 0.0).then_some(cfg.offline_after_min);
                next.offline_after_min = if r.enabled {
                    value.or(current).unwrap_or(DEFAULT_OFFLINE_MIN)
                } else {
                    0.0
                };
            }
            AlertKind::OllamaDown => next.ollama_down = r.enabled,
            AlertKind::GenericText => next.generic_text = r.enabled,
            AlertKind::Unknown => bail!("unknown alert type"),
        }
    }
    Ok(next)
}

/// Changes the running agent's settings and saves `config.toml`.
pub fn set(agent: &Agent, changes: &[AlertRule]) -> Result<Vec<AlertRule>> {
    let mut cfg = agent.config.write().expect("config lock poisoned");
    let alerts = apply(&cfg.alerts, changes)?;
    let mut next = cfg.clone();
    next.alerts = alerts;
    next.save(&agent.paths.config_file())?;
    *cfg = next;
    Ok(rules(&cfg.alerts))
}

pub fn get(agent: &Agent) -> Vec<AlertRule> {
    rules(&agent.config.read().expect("config lock poisoned").alerts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: AlertKind, enabled: bool, threshold: Option<f32>) -> AlertRule {
        AlertRule {
            kind,
            enabled,
            threshold,
        }
    }

    #[test]
    fn generic_text_is_a_setting() {
        let cfg = AlertConfig::default();
        assert!(
            !rules(&cfg)
                .iter()
                .any(|r| r.kind == AlertKind::GenericText && r.enabled)
        );
        let next = apply(&cfg, &[rule(AlertKind::GenericText, true, None)]).unwrap();
        assert!(next.generic_text);
        assert_eq!(
            AlertConfig {
                generic_text: false,
                ..next
            },
            cfg
        );
    }

    #[test]
    fn defaults_follow_the_spec() {
        let r = rules(&AlertConfig::default());
        let by = |k| r.iter().find(|x| x.kind == k).unwrap().clone();
        assert_eq!(by(AlertKind::GpuTemp).threshold, Some(85.0));
        assert_eq!(by(AlertKind::DiskFull).threshold, Some(90.0));
        assert_eq!(by(AlertKind::Offline).threshold, Some(5.0));
        assert!(by(AlertKind::JobEnd).enabled);
        assert!(!by(AlertKind::OllamaDown).enabled);
    }

    #[test]
    fn changes_apply_and_round_trip() {
        let cfg = AlertConfig::default();
        let next = apply(
            &cfg,
            &[
                rule(AlertKind::GpuTemp, true, Some(80.0)),
                rule(AlertKind::Offline, false, None),
                rule(AlertKind::OllamaDown, true, None),
            ],
        )
        .unwrap();
        assert_eq!(next.gpu_temp_c, Some(80.0));
        assert_eq!(next.offline_after_min, 0.0);
        assert!(next.ollama_down);
        // Turning offline back on without a value restores the default.
        let back = apply(&next, &[rule(AlertKind::Offline, true, None)]).unwrap();
        assert_eq!(back.offline_after_min, 5.0);
    }

    #[test]
    fn out_of_range_changes_nothing() {
        let cfg = AlertConfig::default();
        let err = apply(&cfg, &[rule(AlertKind::GpuTemp, true, Some(300.0))]).unwrap_err();
        assert!(err.to_string().contains("between 40 and 110"));
    }
}
