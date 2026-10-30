//! Messages between the `pgpu` command line (or the 1.2 tray app) and the local agent.
//! Local clients are trusted as the same user; this never travels over the network.

use serde::{Deserialize, Serialize};

use crate::msg::control::{Action, AlertRule, NewAction};
use crate::msg::jobs::{JobDetail, JobSummary};
use crate::types::{GpuInfo, Permission, Platform, Sample};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Status,
    /// Starts a pairing code. The agent replies `PairCode`, then `PairAsk` when a phone
    /// proves the code, and ends with `Paired` or `PairEnded`. Closing the stream cancels.
    Pair,
    /// The answer to `PairAsk`: the permission to grant, or `None` to deny.
    PairDecision {
        permission: Option<Permission>,
    },
    /// `pgpu run` registers a job; the agent replies `JobStarted`. The stream stays open
    /// for the job's life: the agent may send `StopJob`, and the wrapper ends with
    /// `JobEnd`. A stream that closes without `JobEnd` marks the job lost.
    JobStart(JobStart),
    /// The job's process group, once the command is running.
    JobProcess {
        pgid: u32,
    },
    JobEnd {
        exit_code: Option<i32>,
        /// Ended because of a stop request.
        stopped: bool,
    },
    ListJobs {
        limit: u32,
    },
    GetJob {
        id: u64,
    },
    StopJob {
        id: u64,
    },
    GetAlerts,
    /// Changes these rules; the agent replies with all rules.
    SetAlerts(Vec<AlertRule>),
    ListActions,
    AddAction(NewAction),
    /// Changes the given fields of the action with this id or name.
    EditAction(ActionEdit),
    RemoveAction {
        action: String,
    },
    /// Audit entries at or after this time, newest first.
    Audit {
        since_ms: i64,
    },
    ListDevices,
    /// `device` is a phone's name or the start of its key.
    SetDevicePermission {
        device: String,
        permission: Permission,
    },
    RevokeDevice {
        device: String,
    },
    /// Terminal access and open sessions; replies `Shell`.
    GetShell,
    /// Turns terminal access on or off; off closes every session. Replies `Shell`.
    SetShell {
        on: bool,
    },
    /// Alert channels (F20); replies `Channels`.
    ListChannels,
    AddChannel(Channel),
    /// By position in `ListChannels`, from 1.
    RemoveChannel {
        number: u32,
    },
    /// Sends a test alert to every channel; replies `ChannelTest`.
    TestChannels,
    /// Custom relay URLs (F19); replies `Relays`. Empty means the default relays.
    GetRelays,
    /// Replaces the relay list; the agent uses it after a restart.
    SetRelays(Vec<String>),
    /// Stops the agent cleanly; replies `Ok`. Windows has no service manager to ask.
    Shutdown,
    /// Holds push and channel alerts for this many minutes (the tray app's "Pause alerts");
    /// 0 resumes them. Not kept across restarts. Replies `Status`.
    PauseAlerts {
        minutes: u32,
    },
    /// Docker for phones (F28) and whether Docker answers; replies `Docker`.
    GetDocker,
    /// Turns Docker for phones on or off; replies `Docker`.
    SetDocker {
        on: bool,
    },
}

/// Where the agent sends alerts besides the push server (F20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    /// A topic on ntfy.sh, or the full URL of a topic on another ntfy server.
    Ntfy {
        topic: String,
    },
    Telegram {
        bot_token: String,
        chat_id: String,
    },
    /// A Discord webhook URL.
    Discord {
        url: String,
    },
    /// Any URL; gets a JSON POST.
    Webhook {
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// The phone's public key (hex).
    pub id: String,
    pub name: String,
    pub platform: String,
    pub permission: Permission,
    pub paired_at_ms: i64,
    pub last_seen_ms: Option<i64>,
    /// Has an open connection right now.
    pub connected: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionEdit {
    /// The action's id or name.
    pub action: String,
    pub name: Option<String>,
    pub command: Option<String>,
    pub icon: Option<String>,
    pub risky: Option<bool>,
    pub timeout_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at_ms: i64,
    /// The phone's name, or `local` for the command line.
    pub device: String,
    pub event: String,
    pub target: String,
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobStart {
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub alerts: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Status(Box<Status>),
    PairCode(PairCodeInfo),
    PairAsk(PairAsk),
    Paired {
        device_name: String,
        permission: Permission,
    },
    /// Pairing ended without a new phone; the text says why.
    PairEnded(String),
    JobStarted {
        id: u64,
        /// Where the wrapper writes the output.
        log_path: String,
    },
    /// To the wrapper: stop the job (SIGTERM, then SIGKILL after 10 s).
    StopJob,
    Jobs(Vec<JobSummary>),
    Job {
        detail: Box<JobDetail>,
        log_path: String,
    },
    Alerts(Vec<AlertRule>),
    Actions(Vec<Action>),
    Action(Action),
    Audit(Vec<AuditEntry>),
    Devices(Vec<DeviceInfo>),
    Device(DeviceInfo),
    Shell {
        enabled: bool,
        /// Open sessions on all phones.
        sessions: u32,
    },
    /// Channels with secrets masked, such as `telegram · chat 12345 · bot 1234…`.
    Channels(Vec<String>),
    Relays(Vec<String>),
    /// Per channel, in order: `None` if the test alert went through, or the error.
    ChannelTest(Vec<(String, Option<String>)>),
    Ok,
    Error(String),
    Docker {
        enabled: bool,
        /// Containers Docker reports, or why it can't be reached.
        containers: Result<u32, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub name: String,
    pub agent_id: String,
    pub fingerprint: String,
    pub version: String,
    pub gpus: Vec<GpuInfo>,
    pub latest: Option<Sample>,
    pub gpu_problem: Option<String>,
    pub paired_devices: u32,
    pub connected_devices: u32,
    /// Of the connected devices, how many reach the machine directly rather than relayed.
    pub direct_devices: u32,
    /// When the agent started, in Unix milliseconds.
    pub started_ms: i64,
    /// Alerts are held until then (`PauseAlerts`).
    pub alerts_paused_until_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairCodeInfo {
    /// Shown as `4F7K-92QD`.
    pub code: String,
    /// What the QR code holds.
    pub qr: String,
    pub expires_in_s: u32,
    pub agent_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairAsk {
    pub device_name: String,
    pub platform: Platform,
    pub device_fingerprint: String,
}
