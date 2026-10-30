//! Runs a real agent and a real client on one machine: local endpoints, no relays, fake GPUs.

#![allow(dead_code)]

use std::sync::Arc;

use iroh::SecretKey;
use iroh::protocol::Router;
use pocketgpu::collect::fake::FakeGpus;
use pocketgpu::daemon::{self, state::Agent};
use pocketgpu::store::devices;
use pocketgpu_client::{Client, Machine, Relays};
use pocketgpu_proto::{Permission, Platform};

pub struct Harness {
    pub agent: Arc<Agent>,
    pub router: Router,
}

impl Harness {
    pub async fn start() -> Self {
        Self::start_with(Arc::new(FakeGpus::new(2))).await
    }

    /// An agent with a custom GPU collector, for tests that need specific GPU processes.
    pub async fn start_with(gpus: Arc<dyn pocketgpu::collect::GpuCollector>) -> Self {
        let agent = Agent::in_memory(gpus).unwrap();
        agent.sampler.take(None);
        let router = daemon::spawn_local(agent.clone()).await.unwrap();
        Self { agent, router }
    }

    /// A phone with a fresh key, paired with `permission` (or not paired when `None`).
    pub async fn phone(&self, permission: Option<Permission>) -> Phone {
        let secret = SecretKey::generate();
        if let Some(p) = permission {
            let db = self.agent.db();
            devices::upsert(
                &db,
                &secret.public().to_string(),
                "Test phone",
                Platform::Ios,
                p,
                None,
            )
            .unwrap();
        }
        let client = Client::new(secret.clone(), Relays::Disabled).await.unwrap();
        Phone {
            client,
            key: secret,
        }
    }

    pub async fn connect(&self, phone: &Phone) -> Result<Machine, pocketgpu_client::ClientError> {
        phone.client.connect(self.router.endpoint().addr()).await
    }

    pub fn audit_events(&self, event: &str) -> usize {
        pocketgpu::audit::since(&self.agent.db(), 0)
            .unwrap()
            .iter()
            .filter(|e| e.event == event)
            .count()
    }
}

pub struct Phone {
    pub client: Client,
    pub key: SecretKey,
}
