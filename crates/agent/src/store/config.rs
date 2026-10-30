//! `config.toml`: settings the user changes with `pgpu` commands or by hand.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::paths::write_private;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Machine name shown on phones. Defaults to the hostname.
    pub name: Option<String>,
    /// Custom relay URLs. Empty means the default relays.
    pub relays: Vec<String>,
    pub ollama_addr: String,
    pub crash_reports: bool,
    /// Terminal access for Shell phones (F16). Off until `pgpu shell on` at the machine.
    pub shell: bool,
    /// Docker containers for phones (F28). Off until `pgpu docker on` at the machine.
    pub docker: bool,
    pub push_url: String,
    pub alerts: AlertConfig,
    /// Alert channels besides push (F20): ntfy, Telegram, Discord, webhooks.
    pub notify: Vec<pocketgpu_proto::local::Channel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertConfig {
    pub job_end: bool,
    pub gpu_temp_c: Option<f32>,
    pub disk_full_pct: Option<f32>,
    pub ollama_down: bool,
    pub offline_after_min: f32,
    /// Push text without job and machine names.
    pub generic_text: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: None,
            relays: Vec::new(),
            ollama_addr: "http://127.0.0.1:11434".into(),
            crash_reports: false,
            shell: false,
            docker: false,
            push_url: pocketgpu_proto::default_push_url().into(),
            alerts: AlertConfig::default(),
            notify: Vec::new(),
        }
    }
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            job_end: true,
            gpu_temp_c: Some(85.0),
            disk_full_pct: Some(90.0),
            ollama_down: false,
            offline_after_min: 5.0,
            generic_text: false,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }

    /// Saves with mode 0600, because the file can hold tokens for alert channels.
    pub fn save(&self, path: &Path) -> Result<()> {
        write_private(path, toml::to_string_pretty(self)?.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::load(&path).unwrap();
        assert_eq!(cfg, Config::default());
        cfg.name = Some("rig-1".into());
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
    }
}
