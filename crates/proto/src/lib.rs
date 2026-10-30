//! Protocol types and framing shared by the PocketGPU agent, command line and apps.

pub mod alpn;
pub mod docker;
pub mod frame;
pub mod local;
pub mod msg;
pub mod pairing;
pub mod push;
pub mod shell;
pub mod tunnel;
pub mod types;

/// Our relays, set when building a release (`PGPU_DEFAULT_RELAYS`, comma-separated URLs, a
/// repository variable). Empty in development builds, which use n0's public relays.
pub fn default_relays() -> Vec<&'static str> {
    option_env!("PGPU_DEFAULT_RELAYS")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .collect()
}

/// The push server, set when building a release (`PGPU_PUSH_URL`); this default otherwise.
pub fn default_push_url() -> &'static str {
    option_env!("PGPU_PUSH_URL")
        .filter(|u| !u.trim().is_empty())
        .unwrap_or("https://push.pocketgpu.app")
}

/// Close reason the agent gives connected phones on a clean stop.
pub const ENDED_REASON: &[u8] = b"ended";

pub use frame::{FrameError, MAX_FRAME, read_frame, write_frame};
pub use types::*;
