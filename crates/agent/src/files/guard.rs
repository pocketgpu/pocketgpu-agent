//! What may be shared or be the Inbox (F40 user story 4). The rules are the client's
//! `pocketgpu_client::folders`, which the app also uses for where synced files land; here
//! is only what this agent adds: its home folder and its own folders.

pub use pocketgpu_client::folders::{Context, inbox, inside, shareable};

/// This agent's home folder and its config, data and runtime folders.
pub fn context(agent: &crate::daemon::state::Agent) -> Context {
    let p = &agent.paths;
    Context {
        home: super::home(),
        agent_dirs: [&p.config_dir, &p.data_dir, &p.runtime_dir]
            .into_iter()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()))
            .collect(),
    }
}
