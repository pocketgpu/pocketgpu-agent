//! Shared state of a running agent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;
use iroh::PublicKey;
use iroh::endpoint::Connection;
use rusqlite::Connection as Db;

use crate::collect::GpuCollector;
use crate::collect::sampler::Sampler;
use crate::jobs::Jobs;
use crate::pair::Pairing;
use crate::store::config::Config;
use crate::store::identity::{Identity, machine_name};
use crate::store::paths::Paths;
use crate::store::{config, db};

/// Open connections one phone may hold; each protocol is its own connection.
const MAX_CONNECTIONS_PER_DEVICE: usize = 32;

pub struct Agent {
    pub paths: Paths,
    pub identity: Identity,
    pub config: RwLock<Config>,
    pub db: Arc<Mutex<Db>>,
    pub sampler: Arc<Sampler>,
    pub pairing: Pairing,
    pub jobs: Jobs,
    /// Actions running right now.
    pub running: crate::actions::Running,
    /// Terminal sessions (F16).
    pub shells: crate::shell::Shells,
    /// Woken by a local `Shutdown` request.
    pub stop: tokio::sync::Notify,
    /// Unix milliseconds.
    pub started_ms: i64,
    /// Alerts are held until this time (Unix ms); 0 when they aren't.
    pub alerts_paused_until: std::sync::atomic::AtomicI64,
    /// Fired alerts, for apps subscribed over `pgpu/events/1`.
    pub events: tokio::sync::broadcast::Sender<pocketgpu_proto::msg::events::Alert>,
    /// Open connections per phone, so revoking can drop them at once.
    connections: Mutex<HashMap<PublicKey, Vec<Connection>>>,
}

impl Agent {
    pub fn open(paths: Paths, gpus: Arc<dyn GpuCollector>) -> Result<Arc<Self>> {
        paths.create_all()?;
        let identity = Identity::load_or_create(&paths.key_file())?;
        let config = config::Config::load(&paths.config_file())?;
        let db = db::open(&paths.db_file())?;
        Ok(Self::assemble(paths, identity, config, db, gpus))
    }

    /// An agent with an in-memory database, for tests.
    pub fn in_memory(gpus: Arc<dyn GpuCollector>) -> Result<Arc<Self>> {
        let dir = std::env::temp_dir().join(format!("pgpu-test-{}", rand_suffix()));
        let paths = Paths::under(dir);
        paths.create_all()?;
        let identity = Identity::load_or_create(&paths.key_file())?;
        Ok(Self::assemble(
            paths,
            identity,
            Config::default(),
            db::open_in_memory()?,
            gpus,
        ))
    }

    fn assemble(
        paths: Paths,
        identity: Identity,
        config: Config,
        db: Db,
        gpus: Arc<dyn GpuCollector>,
    ) -> Arc<Self> {
        Arc::new(Self {
            paths,
            identity,
            config: RwLock::new(config),
            db: Arc::new(Mutex::new(db)),
            sampler: Sampler::new(gpus),
            pairing: Pairing::default(),
            jobs: Jobs::default(),
            running: Default::default(),
            shells: Default::default(),
            stop: tokio::sync::Notify::new(),
            started_ms: crate::store::db::now_ms(),
            alerts_paused_until: std::sync::atomic::AtomicI64::new(0),
            events: tokio::sync::broadcast::channel(32).0,
            connections: Mutex::new(HashMap::new()),
        })
    }

    pub fn name(&self) -> String {
        let cfg = self.config.read().expect("config lock poisoned");
        machine_name(cfg.name.as_deref())
    }

    pub fn db(&self) -> std::sync::MutexGuard<'_, Db> {
        self.db.lock().expect("database lock poisoned")
    }

    pub fn track(&self, device: PublicKey, conn: Connection) {
        let mut map = self.connections.lock().expect("connection lock poisoned");
        let list = map.entry(device).or_default();
        list.retain(|c| c.close_reason().is_none());
        // A phone needs one or two; the oldest goes when one key opens many.
        if list.len() >= MAX_CONNECTIONS_PER_DEVICE {
            list.remove(0).close(3u32.into(), b"too many connections");
        }
        list.push(conn);
    }

    /// Closes every open connection of a device (after revoking it).
    pub fn drop_connections(&self, device: &PublicKey) {
        let conns = self
            .connections
            .lock()
            .expect("connection lock poisoned")
            .remove(device);
        for c in conns.into_iter().flatten() {
            c.close(1u32.into(), b"revoked");
        }
    }

    /// Closes every phone connection, telling the phones why (`ended` on a clean stop).
    pub fn close_all(&self, reason: &[u8]) {
        let all = std::mem::take(&mut *self.connections.lock().expect("connection lock poisoned"));
        for c in all.into_values().flatten() {
            c.close(2u32.into(), reason);
        }
    }

    /// Connected devices whose current path is direct, not through a relay.
    pub fn direct_devices(&self) -> u32 {
        let map = self.connections.lock().expect("connection lock poisoned");
        map.values()
            .filter(|list| {
                list.iter().filter(|c| c.close_reason().is_none()).any(|c| {
                    c.paths()
                        .iter()
                        .find(|p| p.is_selected())
                        .is_some_and(|p| !p.is_relay())
                })
            })
            .count() as u32
    }

    /// Whether this device has an open connection right now.
    pub fn is_connected(&self, device: &PublicKey) -> bool {
        self.connections
            .lock()
            .expect("connection lock poisoned")
            .get(device)
            .is_some_and(|list| list.iter().any(|c| c.close_reason().is_none()))
    }

    pub fn connected_devices(&self) -> u32 {
        let mut map = self.connections.lock().expect("connection lock poisoned");
        map.retain(|_, list| {
            list.retain(|c| c.close_reason().is_none());
            !list.is_empty()
        });
        map.len() as u32
    }
}

fn rand_suffix() -> String {
    iroh::SecretKey::generate().public().to_string()[..12].to_string()
}
