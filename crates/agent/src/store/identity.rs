//! The agent's key pair. Its public key is the agent ID phones dial.

use std::path::Path;

use anyhow::{Context, Result, bail};
use iroh::{PublicKey, SecretKey};

use super::paths::write_private;

#[derive(Debug, Clone)]
pub struct Identity {
    pub secret: SecretKey,
}

impl Identity {
    /// Loads `agent.key`, creating it on first run.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) else {
                    bail!("{} is damaged: expected 32 bytes", path.display());
                };
                Ok(Self {
                    secret: SecretKey::from_bytes(&bytes),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let secret = SecretKey::generate();
                write_private(path, &secret.to_bytes())?;
                Ok(Self { secret })
            }
            Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }

    pub fn agent_id(&self) -> PublicKey {
        self.secret.public()
    }

    /// Shown to people, such as `a91c 40de 77b2 19aa`: the same 64 bits the app shows.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.agent_id())
    }
}

pub fn fingerprint(key: &PublicKey) -> String {
    pocketgpu_proto::pairing::key_groups(&key.to_string())
}

/// Machine name: the configured one, else the hostname, at most 40 characters.
pub fn machine_name(configured: Option<&str>) -> String {
    let name = configured
        .map(str::to_owned)
        .or_else(sysinfo::System::host_name)
        .unwrap_or_else(|| "machine".into());
    name.chars().take(40).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");
        let a = Identity::load_or_create(&path).unwrap();
        let b = Identity::load_or_create(&path).unwrap();
        assert_eq!(a.agent_id(), b.agent_id());
        assert_eq!(a.fingerprint().split(' ').count(), 4);
    }

    #[test]
    fn names_are_capped() {
        assert_eq!(machine_name(Some(&"x".repeat(60))).chars().count(), 40);
    }
}
