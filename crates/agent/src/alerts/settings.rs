//! Alert settings (T073): the rule list phones and `pgpu alerts` see, mapped onto
//! `[alerts]` in `config.toml`.

use anyhow::{Result, bail};
use pocketgpu_proto::msg::control::{AlertKind, AlertRule, IdleAlert};

use crate::daemon::state::Agent;
use crate::store::config::AlertConfig;

pub const DEFAULT_GPU_TEMP_C: f32 = 85.0;
pub const DEFAULT_DISK_PCT: f32 = 90.0;
pub const DEFAULT_OFFLINE_MIN: f32 = 5.0;
pub const DEFAULT_IDLE_MIN: u32 = 30;
/// The idle alert's range in minutes, and the highest hourly price (US cents) it takes.
pub const IDLE_MIN: (u32, u32) = (10, 24 * 60);
pub const MAX_CENTS_PER_HOUR: u32 = 100_000;

/// The idle GPU alert as phones see it (F38).
pub fn idle(cfg: &AlertConfig) -> IdleAlert {
    IdleAlert {
        enabled: cfg.idle_gpu_min.is_some(),
        minutes: cfg.idle_gpu_min.unwrap_or(DEFAULT_IDLE_MIN),
        cents_per_hour: cfg.cents_per_hour,
    }
}

/// Applies a phone's idle alert to the config; refused when out of range.
pub fn apply_idle(cfg: &AlertConfig, a: &IdleAlert) -> Result<AlertConfig> {
    let (lo, hi) = IDLE_MIN;
    if !(lo..=hi).contains(&a.minutes) {
        bail!("the idle time must be between {lo} and {hi} minutes");
    }
    if a.cents_per_hour.is_some_and(|c| c > MAX_CENTS_PER_HOUR) {
        bail!(
            "the hourly price must be at most ${}",
            MAX_CENTS_PER_HOUR / 100
        );
    }
    let mut next = cfg.clone();
    next.idle_gpu_min = a.enabled.then_some(a.minutes);
    next.cents_per_hour = a.cents_per_hour.filter(|&c| c > 0);
    Ok(next)
}

/// Changes the running agent's idle alert and saves `config.toml`.
pub fn set_idle(agent: &Agent, a: &IdleAlert) -> Result<IdleAlert> {
    let mut cfg = agent.config.write().expect("config lock poisoned");
    let alerts = apply_idle(&cfg.alerts, a)?;
    let mut next = cfg.clone();
    next.alerts = alerts;
    next.save(&agent.paths.config_file())?;
    *cfg = next;
    Ok(idle(&cfg.alerts))
}

pub fn rules(cfg: &AlertConfig) -> Vec<AlertRule> {
    vec![
        AlertRule {
            kind: AlertKind::JobEnd,
            enabled: cfg.job_end,
            threshold: None,
        },
        AlertRule {
            kind: AlertKind::Assistant,
            enabled: cfg.assistant,
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
            AlertKind::Assistant => next.assistant = r.enabled,
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

/// The rules as a phone gets them. A phone app from before AI assistant alerts can't read
/// that kind and would lose the whole list, so it gets the list without it; only an app
/// that said it speaks `ASSISTANT_PROTO` or later (in `Hello` or `Status`) sees the rule.
pub fn for_phone(rules: Vec<AlertRule>, proto: Option<u16>) -> Vec<AlertRule> {
    if proto.is_some_and(|p| p >= ASSISTANT_PROTO) {
        return rules;
    }
    rules
        .into_iter()
        .filter(|r| r.kind != AlertKind::Assistant)
        .collect()
}

/// The first proto version whose apps read `AlertKind::Assistant`.
pub const ASSISTANT_PROTO: u16 = 2;

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
    fn idle_alert_is_off_by_default_and_checked() {
        let cfg = AlertConfig::default();
        assert_eq!(
            idle(&cfg),
            IdleAlert {
                enabled: false,
                minutes: DEFAULT_IDLE_MIN,
                cents_per_hour: None
            }
        );
        let on = IdleAlert {
            enabled: true,
            minutes: 45,
            cents_per_hour: Some(190),
        };
        let next = apply_idle(&cfg, &on).unwrap();
        assert_eq!(next.idle_gpu_min, Some(45));
        assert_eq!(idle(&next), on);
        let off = apply_idle(
            &next,
            &IdleAlert {
                enabled: false,
                ..on.clone()
            },
        )
        .unwrap();
        assert_eq!(off.idle_gpu_min, None);
        // A zero price means none; out-of-range values change nothing.
        let free = apply_idle(
            &cfg,
            &IdleAlert {
                cents_per_hour: Some(0),
                ..on.clone()
            },
        )
        .unwrap();
        assert_eq!(free.cents_per_hour, None);
        assert!(
            apply_idle(
                &cfg,
                &IdleAlert {
                    minutes: 5,
                    ..on.clone()
                }
            )
            .is_err()
        );
        assert!(
            apply_idle(
                &cfg,
                &IdleAlert {
                    minutes: 2000,
                    ..on.clone()
                }
            )
            .is_err()
        );
        assert!(
            apply_idle(
                &cfg,
                &IdleAlert {
                    cents_per_hour: Some(MAX_CENTS_PER_HOUR + 1),
                    ..on
                }
            )
            .is_err()
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
    fn assistant_alerts_are_on_by_default_and_switch_alone() {
        let cfg = AlertConfig::default();
        let r = rules(&cfg);
        assert_eq!(r[0].kind, AlertKind::JobEnd);
        // Right under job alerts, like the row on the phone.
        assert_eq!(r[1].kind, AlertKind::Assistant);
        assert!(r[1].enabled && r[1].threshold.is_none());
        let off = apply(&cfg, &[rule(AlertKind::Assistant, false, None)]).unwrap();
        assert!(!off.assistant);
        // Job alerts keep coming.
        assert_eq!(
            AlertConfig {
                assistant: true,
                ..off.clone()
            },
            cfg
        );
        let on = apply(&off, &[rule(AlertKind::Assistant, true, None)]).unwrap();
        assert!(on.assistant);
    }

    #[test]
    fn older_phones_get_the_list_without_the_assistant_rule() {
        let all = rules(&AlertConfig::default());
        let has = |list: &[AlertRule]| list.iter().any(|r| r.kind == AlertKind::Assistant);
        for proto in [None, Some(0), Some(1)] {
            let list = for_phone(all.clone(), proto);
            assert!(!has(&list), "{proto:?}");
            assert_eq!(list.len(), all.len() - 1);
        }
        assert_eq!(for_phone(all.clone(), Some(ASSISTANT_PROTO)), all);
        assert_eq!(for_phone(all.clone(), Some(ASSISTANT_PROTO + 1)), all);
    }

    #[test]
    fn this_app_build_gets_the_assistant_rule() {
        // The phone app sends `PROTO_VERSION`; if it fell behind the gate, the app would
        // never see the AI assistants switch.
        const { assert!(pocketgpu_proto::msg::PROTO_VERSION >= ASSISTANT_PROTO) };
        let all = rules(&AlertConfig::default());
        let list = for_phone(all.clone(), Some(pocketgpu_proto::msg::PROTO_VERSION));
        assert_eq!(list, all);
    }

    #[test]
    fn out_of_range_changes_nothing() {
        let cfg = AlertConfig::default();
        let err = apply(&cfg, &[rule(AlertKind::GpuTemp, true, Some(300.0))]).unwrap_err();
        assert!(err.to_string().contains("between 40 and 110"));
    }
}
