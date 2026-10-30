//! PocketGPU agent: the daemon that serves phones, and the `pgpu` command line.

pub mod actions;
pub mod alerts;
pub mod audit;
pub mod cli;
pub mod collect;
pub mod daemon;
pub mod docker;
pub mod games;
pub mod ipc;
pub mod jobs;
#[cfg(target_os = "macos")]
pub mod macsys;
pub mod pair;
pub mod release;
pub mod shell;
pub mod store;
pub mod tunnel;
#[cfg(windows)]
pub mod winsys;

/// The agent's own environment, kept from what phones start (terminals and actions): test
/// switches, the restricted-token marker and the service manager's variables.
pub const AGENT_ONLY_ENV: &[&str] = &[
    "PGPU_FAKE_GPU",
    "PGPU_UNELEVATED",
    "RUST_LOG",
    "INVOCATION_ID",
    "JOURNAL_STREAM",
];
