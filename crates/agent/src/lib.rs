//! PocketGPU agent: the daemon that serves phones, and the `pgpu` command line.

pub mod actions;
pub mod ai;
pub mod alerts;
pub mod apps;
pub mod audit;
pub mod cli;
pub mod clip;
pub mod collect;
pub mod daemon;
pub mod desktop_app;
pub mod docker;
pub mod files;
pub mod games;
pub mod ipc;
pub mod jobs;
#[cfg(target_os = "macos")]
pub mod macsys;
pub mod notices;
pub mod pair;
pub mod power;
pub mod release;
pub mod screen;
pub mod shell;
pub mod store;
pub mod tunnel;
#[cfg(windows)]
pub mod winsys;

/// The agent's own environment, kept from what phones start (terminals and actions): test
/// switches, the restricted-token and service markers and the service manager's variables,
/// which also tell the service's agent from one started in its terminal
/// (`cli::service::is_this_process`).
pub const AGENT_ONLY_ENV: &[&str] = &[
    "PGPU_FAKE_GPU",
    "PGPU_FAKE_POWER",
    "PGPU_CLIPBOARD",
    "PGPU_UNELEVATED",
    "PGPU_SERVICE",
    "RUST_LOG",
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "XPC_SERVICE_NAME",
];
