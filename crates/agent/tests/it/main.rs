//! The agent's tests that can share one process, as one test program: each file under
//! `tests/` would be its own program with the whole agent linked in, built and stored again.
//! `clip_fallback.rs` and `local.rs` stay on their own: they change process-wide settings
//! (the clipboard and the data folder) that the others rely on.

mod harness;

mod actions;
mod alerts;
mod answer;
mod apps;
mod auth;
mod cli_snapshots;
mod clip;
mod crash;
mod devices_remote;
mod docker;
mod ended;
mod files;
mod files_offers;
mod files_send;
mod files_transfers;
mod footprint;
mod guests;
mod history;
mod jobs;
mod lan_pair;
mod lan_wifi;
mod notify;
mod pair;
mod power;
mod revoke;
mod screen;
mod settings;
mod shares;
mod shell;
mod sync;
mod tunnel;
mod update;
