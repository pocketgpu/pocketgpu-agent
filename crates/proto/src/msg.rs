//! Request and response messages per protocol, as listed in `contracts/protocol.md`.

use serde::{Deserialize, Serialize};

use crate::types::{Error, MachineInfo, Permission, Platform, Sample};

/// The proto version the app speaks, sent in `Hello`.
pub const PROTO_VERSION: u16 = 1;

pub mod pair {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PairRequest {
        /// Keyed hash of the pairing secret and both public keys.
        pub secret_proof: [u8; 32],
        pub device_name: String,
        pub platform: Platform,
        pub push_ticket: Option<String>,
    }

    /// The agent's replies, in order: `Pending` once the proof checks out, then one of
    /// `Approved`, `Denied` or `Error`. A wrong, used or expired code gets `Error` at once.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub enum Response {
        /// Waiting for someone at the machine to approve. Phones that typed the code learn
        /// the machine's ID here and show its fingerprint.
        Pending {
            agent_id: [u8; 32],
            agent_name: String,
        },
        Approved {
            permission: Permission,
            /// Relay URLs to reach the machine later; empty means the default relays.
            relays: Vec<String>,
        },
        Denied,
        Error(Error),
    }
}

pub mod control {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Range {
        H1,
        H6,
        H24,
        D7,
        D30,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct MetricMinute {
        pub at_ms: u64,
        pub gpu: u32,
        pub util_min: u8,
        pub util_avg: u8,
        pub util_max: u8,
        pub vram_max_mb: u64,
        pub temp_max_c: u8,
    }

    /// One minute of the whole machine: CPU, RAM and its main disk, each 0-100 (board 22's
    /// CPU/RAM/disk history, for machines without a GPU and alongside one that has).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct SystemMinute {
        pub at_ms: u64,
        pub cpu_min: u8,
        pub cpu_avg: u8,
        pub cpu_max: u8,
        pub ram_min: u8,
        pub ram_avg: u8,
        pub ram_max: u8,
        pub disk_min: u8,
        pub disk_avg: u8,
        pub disk_max: u8,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Proc {
        pub pid: u32,
        pub user: String,
        pub command: String,
        pub gpu: u32,
        pub vram_mb: u64,
        /// False when the process belongs to another user.
        pub killable: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Action {
        pub id: u64,
        pub name: String,
        pub command: String,
        pub icon: String,
        pub risky: bool,
        pub timeout_s: u32,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct NewAction {
        pub name: String,
        pub command: String,
        pub icon: String,
        pub risky: bool,
        pub timeout_s: Option<u32>,
    }

    /// One run of an action, for the recent runs list (output is not included).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct ActionRun {
        pub id: u64,
        pub action_id: u64,
        pub action_name: String,
        /// The phone that ran it, or `None` for the command line.
        pub device_name: Option<String>,
        pub started_ms: u64,
        pub ended_ms: Option<u64>,
        /// `None` while running, or when it was stopped by a signal or the timeout.
        pub exit_code: Option<i32>,
        pub timed_out: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum AlertKind {
        JobEnd,
        GpuTemp,
        Offline,
        OllamaDown,
        DiskFull,
        Unknown,
        /// Not an alert: when enabled, pushes leave out job names and details (FR-023).
        /// Last, so the other kinds keep their wire numbers.
        GenericText,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct AlertRule {
        pub kind: AlertKind,
        pub enabled: bool,
        /// °C for GpuTemp, % for DiskFull, minutes for Offline; unused otherwise.
        pub threshold: Option<f32>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub enum Request {
        Hello {
            app_version: String,
            proto: u16,
        },
        GetHistory {
            range: Range,
            gpu: u32,
        },
        /// CPU, RAM and disk history (board 22): agents before it answer with an error, and
        /// the phone shows no range beyond the live 10 minutes, as before.
        GetSystemHistory {
            range: Range,
        },
        ListProcesses,
        KillProcess {
            pid: u32,
        },
        ListActions,
        RunAction {
            id: u64,
        },
        CreateAction(NewAction),
        EditAction {
            id: u64,
            action: NewAction,
        },
        DeleteAction {
            id: u64,
        },
        GetAlertSettings,
        SetAlertSettings {
            rules: Vec<AlertRule>,
        },
        SetPushTicket {
            ticket: String,
        },
        Unpair,
        /// Newest first.
        ListRuns {
            limit: u32,
        },
        /// Games running on the machine, the main one first (5.1 now playing).
        ListGames,
        /// Everything the machines list shows, in one reply: `Hello`'s answer, the newest
        /// sample, the running job and the games. Saves round trips on far relays; agents
        /// before it answer with an error, and the phone asks the old way.
        Status {
            app_version: String,
            proto: u16,
        },
        /// The one-second samples the agent keeps in memory (the last 10 minutes), oldest
        /// first, without disks. Agents before it answer with an error, and the phone starts
        /// from the per-minute history instead.
        GetRecent,
        /// Whether a newer agent release is out for this machine (board 23). Agents before
        /// it answer with an error, and the phone shows no update.
        CheckUpdate,
        /// Installs this signed agent release, then restarts the agent (Shell only). The
        /// reply comes before the restart.
        InstallUpdate {
            version: String,
        },
        /// The machine's paired devices, newest first (F35). Agents before it answer with
        /// an error.
        ListDevices,
        /// Removes another device and closes its connections (Shell). Refused for the
        /// caller itself (`Unpair`) and for the computer's own app.
        RevokeDevice {
            device: String,
        },
        /// Whether power actions are on here, what the system allows and whether the machine
        /// comes back by itself (F37). Agents before it answer with an error.
        GetPower,
        /// Restart, sleep or shut down after a 30-second countdown on the machine (Control,
        /// and only when turned on there). Replies `PowerScheduled`.
        PowerAction {
            action: PowerKind,
        },
    }

    /// F37: what a phone may ask the machine to do with its power.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum PowerKind {
        Restart,
        Sleep,
        ShutDown,
    }

    impl PowerKind {
        pub fn as_str(self) -> &'static str {
            match self {
                Self::Restart => "restart",
                Self::Sleep => "sleep",
                Self::ShutDown => "shut down",
            }
        }
    }

    /// Whether the agent starts again by itself after a restart.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ComesBack {
        Yes,
        /// Windows waits at the sign-in screen: automatic sign-in is off.
        NoAutoSignIn,
        /// Linux: the agent's service starts at login only (no lingering).
        NoLinger,
        Unknown,
    }

    /// The last power action and how it went.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PowerOutcome {
        pub action: PowerKind,
        /// Who asked: a device's name, or "this computer".
        pub by: String,
        pub at_ms: i64,
        /// `started`, `cancelled`, or why the system refused.
        pub result: String,
    }

    /// The answer to `GetPower`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PowerState {
        pub enabled: bool,
        pub comes_back: ComesBack,
        /// What the system allows the agent's user; the rest is refused up front.
        pub allowed: Vec<PowerKind>,
        /// The action counting down, and when it runs (ms).
        pub pending: Option<(PowerKind, i64)>,
        pub last: Option<PowerOutcome>,
    }

    /// A paired device, as a phone sees it (F35).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct DeviceSummary {
        /// The device's public key (hex).
        pub id: String,
        pub name: String,
        pub platform: String,
        pub permission: Permission,
        pub paired_at_ms: i64,
        pub last_seen_ms: Option<i64>,
        pub connected: bool,
        /// The computer's own PocketGPU app: shown, never revocable from a phone.
        pub this_pc: bool,
        /// The device asking.
        pub you: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub enum Response {
        Ok,
        MachineInfo(MachineInfo),
        History(Vec<MetricMinute>),
        SystemHistory(Vec<SystemMinute>),
        Processes(Vec<Proc>),
        Actions(Vec<Action>),
        Action(Action),
        ActionResult {
            exit_code: Option<i32>,
            output: String,
            duration_ms: u64,
            timed_out: bool,
        },
        Runs(Vec<ActionRun>),
        AlertSettings(Vec<AlertRule>),
        Error(Error),
        Games(Vec<Game>),
        Status(Box<Status>),
        Recent(Vec<Sample>),
        Update(UpdateCheck),
        /// The new binary is in place; `restarting` is false when the agent can't restart
        /// itself and someone has to on the machine.
        Installed {
            version: String,
            restarting: bool,
        },
        Devices(Vec<DeviceSummary>),
        Revoked {
            device: String,
        },
        Power(PowerState),
        /// The action runs at this time unless someone at the machine cancels.
        PowerScheduled {
            at_ms: i64,
        },
    }

    /// The answer to `CheckUpdate`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct UpdateCheck {
        pub current: String,
        /// The newest release's version, without the `v`; `None` when it couldn't be looked
        /// up.
        pub latest: Option<String>,
        /// `latest` is newer than `current`.
        pub available: bool,
        /// Why this machine can't update from a phone (a container, no service), in words.
        pub blocked: Option<String>,
    }

    /// The answer to `Status`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Status {
        pub info: MachineInfo,
        pub sample: Option<Sample>,
        /// The newest running job, if any.
        pub running: Option<super::jobs::JobSummary>,
        pub games: Vec<Game>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Store {
        Steam,
        Epic,
        Xbox,
    }

    /// A game running on the machine, found from the stores' install records.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Game {
        /// Steam's app id, Epic's app name or Xbox's folder: stable while it's installed.
        pub id: String,
        pub name: String,
        pub store: Store,
        /// The process to close it (the one with the most memory).
        pub pid: u32,
        /// When its first process started.
        pub started_ms: i64,
        /// The GPU it draws on and the video memory it holds, when the driver says.
        pub gpu: Option<u32>,
        pub vram_mb: Option<u64>,
    }

    impl Request {
        /// The lowest permission allowed to send this request.
        pub fn min_permission(&self) -> Permission {
            match self {
                Self::Hello { .. }
                | Self::Status { .. }
                | Self::GetHistory { .. }
                | Self::GetSystemHistory { .. }
                | Self::GetRecent
                | Self::ListActions
                | Self::ListRuns { .. }
                | Self::ListGames
                | Self::GetAlertSettings
                | Self::SetPushTicket { .. }
                | Self::CheckUpdate
                | Self::GetPower
                | Self::Unpair => Permission::View,
                Self::ListProcesses
                | Self::KillProcess { .. }
                | Self::ListDevices
                | Self::PowerAction { .. }
                | Self::RunAction { .. }
                | Self::SetAlertSettings { .. } => Permission::Control,
                // Replacing the agent's own program is as much as terminal access.
                Self::CreateAction(_)
                | Self::EditAction { .. }
                | Self::DeleteAction { .. }
                | Self::InstallUpdate { .. }
                | Self::RevokeDevice { .. } => Permission::Shell,
            }
        }
    }
}

pub mod metrics {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Subscribe {
        pub interval_ms: u32,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub enum Event {
        Sample(Sample),
        Error(Error),
    }
}

/// `pgpu/events/1`: the app sends `Subscribe` once, then the agent sends an `Alert` each time
/// one fires (the same alerts as push and channels) until the app closes the stream.
pub mod events {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Subscribe;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Alert {
        pub title: String,
        pub body: String,
        /// `job_end`, `gpu_temp`, `disk_full`, `ollama_down` or `test`.
        pub kind: String,
        /// The job, for job alerts.
        pub job: Option<u64>,
        /// Unix milliseconds.
        pub at_ms: u64,
    }
}

pub mod jobs {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum JobState {
        Running,
        Finished,
        Crashed,
        Stopped,
        Lost,
        Unknown,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct JobSummary {
        pub id: u64,
        pub name: String,
        pub state: JobState,
        pub started_ms: u64,
        pub ended_ms: Option<u64>,
        pub exit_code: Option<i32>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct JobDetail {
        pub summary: JobSummary,
        pub command: String,
        pub peak_vram_mb: Option<u64>,
        pub log_bytes: u64,
        pub alerts_on: bool,
        /// Last 20 lines of output.
        pub tail: Vec<String>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub enum Request {
        ListJobs { limit: u32, before: Option<u64> },
        GetJob { id: u64 },
        FollowLog { id: u64, from_offset: u64 },
        StopJob { id: u64 },
        SetJobAlerts { id: u64, on: bool },
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub enum Response {
        Ok,
        Jobs(Vec<JobSummary>),
        Job(JobDetail),
        LogChunk { offset: u64, bytes: Vec<u8> },
        End { exit_code: Option<i32> },
        Error(Error),
    }

    impl Request {
        pub fn min_permission(&self) -> Permission {
            match self {
                Self::ListJobs { .. } | Self::GetJob { .. } | Self::FollowLog { .. } => {
                    Permission::View
                }
                Self::StopJob { .. } | Self::SetJobAlerts { .. } => Permission::Control,
            }
        }
    }
}
