//! Protocol names. The version is part of the name; a breaking change adds a new ALPN.

/// Pairing. The only protocol open to unknown keys, and only while a pairing code is active.
pub const PAIR: &[u8] = b"pgpu/pair/1";
/// Pairing a computer found on the same Wi-Fi (F47): no secret, so open only while a pairing
/// session runs, only on a direct private path, and only with a yes at the computer
/// (`specs/F47-find-on-wifi/contracts/lan.md` §2).
pub const PAIR_LAN: &[u8] = b"pgpu/pair-lan/1";
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

/// Files: shared folders, the Inbox and offers, one stream per request or transfer (F40).
pub const FILES: &[u8] = b"pgpu/files/1";

/// Text to and from the machine's clipboard, one stream per request (F41).
pub const CLIP: &[u8] = b"pgpu/clip/1";

/// The machine's web apps and a way in to them, one stream per TCP connection (F42).
pub const APPS: &[u8] = b"pgpu/apps/1";

/// A live picture of the machine's screen, one stream per look (F45).
pub const SCREEN: &[u8] = b"pgpu/screen/1";

/// Share a model (F48): the only protocol a guest key may use. Unknown keys may only claim a
/// share with its link's secret; a claimed key may only chat with that share's model
/// (`specs/F48-share-a-model/contracts/model-guest.md`).
pub const MODEL_GUEST: &[u8] = b"pgpu/model-guest/1";

/// Every protocol the agent serves.
pub const ALL: [&[u8]; 14] = [
    PAIR,
    CONTROL,
    METRICS,
    JOBS,
    SHELL,
    TUNNEL,
    EVENTS,
    DOCKER,
    FILES,
    CLIP,
    APPS,
    SCREEN,
    PAIR_LAN,
    MODEL_GUEST,
];
