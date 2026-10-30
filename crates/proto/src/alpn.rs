//! Protocol names. The version is part of the name; a breaking change adds a new ALPN.

/// Pairing. The only protocol open to unknown keys, and only while a pairing code is active.
pub const PAIR: &[u8] = b"pgpu/pair/1";
/// Request and reply: machine info, processes, actions, alert settings.
pub const CONTROL: &[u8] = b"pgpu/control/1";
/// One stream per app session with a sample every second.
pub const METRICS: &[u8] = b"pgpu/metrics/1";
/// Jobs: list, detail, log streaming and stop.
pub const JOBS: &[u8] = b"pgpu/jobs/1";
/// Terminal sessions, one stream each (1.1).
pub const SHELL: &[u8] = b"pgpu/shell/1";
/// Requests to the machine's Ollama, one stream each (1.1).
pub const TUNNEL: &[u8] = b"pgpu/tunnel/1";

/// Alerts as they fire, to apps that are open (the desktop client's notifications, F31).
pub const EVENTS: &[u8] = b"pgpu/events/1";

/// Docker containers, one stream each (F28).
pub const DOCKER: &[u8] = b"pgpu/docker/1";

/// Every protocol the agent serves.
pub const ALL: [&[u8]; 8] = [PAIR, CONTROL, METRICS, JOBS, SHELL, TUNNEL, EVENTS, DOCKER];
