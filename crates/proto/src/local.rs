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
    /// A short note to the owner's phones (never guests) and alert channels, as an AI
    /// assistant alert (`pgpu mcp`'s notify tool). Replies `Notified`.
    Notify {
        title: String,
        body: String,
    },
    /// Crash reports (board 25): `Some` turns them on or off, `None` only asks. Replies
    /// `CrashReports`.
    CrashReports {
        on: Option<bool>,
    },
    /// `pgpu run` again, after the agent restarted (an update, a crash): picks up job `id`,
    /// which kept running and writing its log. Replies `JobStarted`, then the stream goes on
    /// as after `JobStart`; an error if the agent no longer has it as running.
    JobResume {
        id: u64,
        pgid: Option<u32>,
    },
    /// The PocketGPU app on this same PC pairs itself (F34), with no code: the local socket
    /// already lets the person at this PC approve any phone. `device` is the app's public key
    /// (64 hex characters), `name` at most 40 characters. A new device gets Shell; one already
    /// paired keeps its permission. Replies `PairedLocal`.
    PairLocal {
        device: String,
        name: String,
    },
    /// Devices with an open connection now, without this PC's own app (F35). Replies
    /// `Connected`.
    Connected,
    /// The notice level (F35). Replies `Notices`.
    GetNotices,
    /// Sets the notice level; only possible here, never from a phone. Replies `Notices`.
    SetNotices(NoticeLevel),
    /// Power actions from phones (F37). Replies `Power`.
    GetPower,
    /// Turns power actions from phones on or off; only here. Replies `Power`.
    SetPower {
        on: bool,
    },
    /// Stops a restart, sleep or shutdown that is counting down. Replies `Ok`.
    CancelPower,
    /// Files for other devices (F40). Replies `Files`.
    GetFiles,
    /// Turns files on or off; only here, never from a phone. Replies `Files`.
    SetFiles {
        on: bool,
    },
    /// Shares a folder (an absolute path) with devices that can run actions. Replies `Files`.
    ShareFolder {
        path: String,
    },
    /// Stops sharing a folder (as shared, or an absolute path). Replies `Files`.
    UnshareFolder {
        path: String,
    },
    /// Moves the Inbox to this folder (an absolute path). Replies `Files`.
    SetInbox {
        path: String,
    },
    /// Files in the Inbox, newest first. Replies `InboxList`.
    Inbox,
    /// `pgpu send`: offers a file (an absolute path) for 7 days to the owner's devices with
    /// Control, or to the one named (a device's name or the start of its key); never to
    /// guests, and never keys or secrets. Replies `Offered`.
    Offer {
        path: String,
        to: Option<String>,
    },
    /// The clipboard for other devices (F41). Replies `Clip`.
    GetClip,
    /// Turns the clipboard on or off; only here. Replies `Clip`.
    SetClip {
        on: bool,
    },
    /// What is on the clipboard (`pgpu clip`); reading it counts as a read for `once`.
    /// Replies `ClipText`.
    ClipGet,
    /// Puts text there (`pgpu clip put`); `once` clears the in-memory clipboard after its
    /// first read. Replies `Clip`.
    ClipPut {
        text: String,
        once: bool,
    },
    /// Empties the clipboard. Replies `Clip`.
    ClipClear,
    /// The web apps running here and whether other devices may open them (F42). Replies
    /// `Apps`, with every app found even while apps are off.
    GetApps,
    /// Lets other devices open apps here, or not; only here. Replies `Apps`.
    SetApps {
        on: bool,
    },
    /// Looks for an app on this port too, with this name (`pgpu apps add`). Replies `Apps`.
    AddApp {
        port: u16,
        name: Option<String>,
    },
    /// Stops looking on a port added with `AddApp`. Replies `Apps`.
    RemoveApp {
        port: u16,
    },
    /// Keeps other devices out of one app (`off`), or lets them in again. Replies `Apps`.
    SetAppOff {
        port: u16,
        off: bool,
    },
    /// Whether other devices may see this screen, and who looks now (F45). Replies `Screen`.
    GetScreen,
    /// Lets devices with Full control see this screen, or not; only here. Turning it off
    /// stops everyone looking. Replies `Screen`.
    SetScreen {
        on: bool,
    },
    /// Stops one device looking now (its key), or everyone (`None`). Replies `Screen`.
    StopScreen {
        device: Option<String>,
    },
    /// The model servers here besides Ollama, and their models (F18). Replies `ModelServers`.
    GetModelServers,
    /// Looks for a model server on this port too, with this name (`pgpu ai add`). Replies
    /// `ModelServers`.
    AddModelServer {
        port: u16,
        name: Option<String>,
    },
    /// Stops looking on a port added with `AddModelServer`. Replies `ModelServers`.
    RemoveModelServer {
        port: u16,
    },
    /// Whether this computer can be found on the same Wi-Fi (F47). Replies `Lan`.
    GetLan,
    /// Turns being found on the Wi-Fi on or off (`pgpu lan on|off`). Replies `Lan`.
    SetLan {
        on: bool,
    },
    /// `pgpu hook <assistant>` (F32): an AI assistant's hook saw a turn start, a question for
    /// the user or a turn end. The agent decides whether the phone hears about it. Only the
    /// assistant's short name and its session id travel, never anything it wrote. Replies
    /// `Ok` at once; any push goes out later.
    AssistantEvent {
        assistant: String,
        session: String,
        event: AssistantEventKind,
    },
    /// `pgpu hook <assistant> --check` (F32), before every tool call: holds the action for
    /// the phone when "Ask me first" matches. Two replies: `AssistantAnswer(Pass)` or
    /// `AssistantHeld` within 1 s, then the final `AssistantAnswer` for a held one.
    AssistantCheck {
        assistant: String,
        session: String,
        /// Claude Code's `permission_mode`; the fail-closed answer depends on it.
        permission_mode: String,
        tool: String,
        /// At most `assistants::INPUT_MAX` bytes.
        input: String,
        /// The input was over `INPUT_MAX` or didn't parse.
        oversize: bool,
        folder: String,
    },
    /// `pgpu hook <assistant> --ask` (F32), when the assistant would ask at the desk; replies
    /// as `AssistantCheck`.
    AssistantAsk {
        assistant: String,
        session: String,
        tool: String,
        input: String,
        folder: String,
    },
    /// The `[assistants]` settings (F32, `pgpu assistants set`). Replies `AssistantSettings`.
    GetAssistantSettings,
    /// Changes them at the computer; audited and the guard file rewritten. Replies
    /// `AssistantSettings`, or `Error` when they don't validate.
    SetAssistantSettings(crate::assistants::AssistantSettings),
    /// `pgpu mcp --assistant <id>` (F32): a tool that needs more than View. The agent checks
    /// the assistant's level (missing means View) and audits a refusal as `assistant_refused`.
    /// `folder` and `since_ms` are the MCP server's folder and start: `stop_job` stops only
    /// jobs started there since. Replies `Offered`, `Ok`, `ActionRan` or `Error`.
    AssistantTool {
        assistant: String,
        folder: String,
        since_ms: i64,
        call: crate::assistants::ToolCall,
    },
}

/// What an AI assistant's hook saw (F32). Append only: postcard sends the position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssistantEventKind {
    /// The user sent a prompt.
    TurnStarted,
    /// It asks permission for something; the user may still answer at the desk.
    NeedsYou,
    /// It has waited a while for the user's answer.
    Idle,
    /// It stopped and handed the turn back.
    TurnEnded,
    /// It used a tool, so it is working again: a permission question it asked was answered
    /// (Claude Code says nothing when the user approves one at the desk).
    Working,
    /// A tool call ended (`PostToolUse` or `PostToolUseFailure`): also working again, and the
    /// desk answered a held "Ask on my phone" request with this hash of the tool and its
    /// input (`assistants::answers::input_hash` on the agent). An agent from before it
    /// refuses the request; the hook doesn't mind.
    ToolDone { input_hash: [u8; 32] },
}

/// How many remote actions show a notice on this computer (F35). There is no off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    /// New devices, terminals, files, the clipboard, stopped processes, quick actions and
    /// containers.
    #[default]
    All,
    /// Only new devices, terminals, files sent here and clipboard reads.
    Fewer,
}

impl NoticeLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Fewer => "fewer",
        }
    }
}

/// What a connected device is doing (F35).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Activity {
    Watching,
    Terminal,
    /// Looking at the screen (F45).
    Screen,
}

/// How a connected device reaches this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Via {
    Direct,
    Relay,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectedDevice {
    /// The device's public key (hex).
    pub id: String,
    pub name: String,
    pub platform: String,
    pub activity: Activity,
    pub via: Via,
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
    /// How a `Notify` went: phones the push server delivered to, and alert channels that
    /// took it. `held` when alerts are paused or AI assistant alerts are off.
    Notified {
        phones: u32,
        channels: u32,
        held: bool,
    },
    /// Whether crash reports are on.
    CrashReports {
        on: bool,
    },
    /// The app on this PC is paired: what it needs to connect like to any machine.
    PairedLocal {
        agent_id: String,
        name: String,
        relays: Vec<String>,
    },
    Connected(Vec<ConnectedDevice>),
    Notices(NoticeLevel),
    Power(crate::msg::control::PowerState),
    Files(FilesStatus),
    InboxList(Vec<InboxFile>),
    /// The file is offered; `to` names the devices that may save it, `pushed` those of
    /// them that get a push (the others have no push ticket here).
    Offered {
        name: String,
        size: u64,
        to: Vec<String>,
        expires_ms: i64,
        pushed: Vec<String>,
    },
    Clip(ClipStatus),
    /// `None` when the clipboard is empty or holds no text.
    ClipText(Option<String>),
    Apps(crate::apps::Apps),
    Screen(ScreenStatus),
    ModelServers(Vec<crate::ai::ModelServer>),
    Lan(LanStatus),
    /// The action waits for the phone; the final `AssistantAnswer` follows (F32).
    AssistantHeld {
        id: u64,
    },
    AssistantAnswer(crate::assistants::Answer),
    AssistantSettings(crate::assistants::AssistantSettings),
    /// A saved action ran for an assistant's `run_action` (F32).
    ActionRan {
        exit_code: Option<i32>,
        output: String,
        duration_ms: u64,
        timed_out: bool,
    },
}

/// Being found on the same Wi-Fi (F47).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanStatus {
    /// Shown on the Wi-Fi at all (`pgpu lan on`).
    pub on: bool,
    /// A `pgpu pair` session waits: the name shows, and phones there can ask to pair.
    pub waiting: bool,
}

/// The screen for other devices (F45), as `pgpu screen` and This PC show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenStatus {
    pub enabled: bool,
    /// Whether this machine has a screen to show, and why not.
    pub why: Option<String>,
    /// Who looks now.
    pub watching: Vec<ScreenWatcher>,
}

/// A device looking at the screen now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenWatcher {
    pub device: String,
    pub name: String,
    pub display: u32,
    pub since_ms: i64,
}

/// Files for other devices (F40), as `pgpu files` shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesStatus {
    pub enabled: bool,
    /// Shared folders, as people know them (`~/ComfyUI/output`).
    pub shared: Vec<String>,
    pub inbox: String,
    pub inbox_files: u32,
    /// Offers that haven't expired.
    pub offers: u32,
}

/// A file another device sent here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxFile {
    pub name: String,
    pub size: u64,
    /// The device's name, or empty when the agent no longer knows it.
    pub from: String,
    /// When it arrived, in Unix milliseconds.
    pub at_ms: i64,
}

/// The clipboard for other devices (F41), as `pgpu clip status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipStatus {
    pub enabled: bool,
    pub place: crate::clip::Place,
    /// There is text to read.
    pub has_text: bool,
    /// When the in-memory clipboard forgets its text.
    pub clears_in_s: Option<u32>,
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
    /// Found on this Wi-Fi (F47), not by a code: always asked, never approved in advance.
    pub from_lan: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first byte postcard writes for an enum is the variant's position.
    fn tag<T: Serialize>(v: &T) -> u8 {
        postcard::to_allocvec(v).unwrap()[0]
    }

    #[test]
    fn assistant_events_keep_their_numbers() {
        let req = Request::AssistantEvent {
            assistant: "claude".into(),
            session: "abc".into(),
            event: AssistantEventKind::NeedsYou,
        };
        assert_eq!(tag(&req), 66, "AssistantEvent moved");
        let kinds = [
            AssistantEventKind::TurnStarted,
            AssistantEventKind::NeedsYou,
            AssistantEventKind::Idle,
            AssistantEventKind::TurnEnded,
            AssistantEventKind::Working,
        ];
        for (n, kind) in kinds.into_iter().enumerate() {
            assert_eq!(tag(&kind) as usize, n, "{kind:?} moved");
        }
        let bytes = postcard::to_allocvec(&req).unwrap();
        assert_eq!(postcard::from_bytes::<Request>(&bytes).unwrap(), req);
    }

    #[test]
    fn assistant_approvals_keep_their_numbers() {
        let check = Request::AssistantCheck {
            assistant: "claude".into(),
            session: "s".into(),
            permission_mode: "default".into(),
            tool: "Bash".into(),
            input: "rm -rf build".into(),
            oversize: false,
            folder: "/w".into(),
        };
        assert_eq!(tag(&check), 67, "AssistantCheck moved");
        let ask = Request::AssistantAsk {
            assistant: "claude".into(),
            session: "s".into(),
            tool: "Bash".into(),
            input: "ls".into(),
            folder: "/w".into(),
        };
        assert_eq!(tag(&ask), 68, "AssistantAsk moved");
        let held = Response::AssistantHeld { id: 7 };
        assert_eq!(tag(&held), 37, "AssistantHeld moved");
        let answer = Response::AssistantAnswer(crate::assistants::Answer::Allow);
        assert_eq!(tag(&answer), 38, "AssistantAnswer moved");
        for msg in [check, ask] {
            let bytes = postcard::to_allocvec(&msg).unwrap();
            assert_eq!(postcard::from_bytes::<Request>(&bytes).unwrap(), msg);
        }
        let bytes = postcard::to_allocvec(&answer).unwrap();
        assert_eq!(postcard::from_bytes::<Response>(&bytes).unwrap(), answer);
    }

    #[test]
    fn assistant_settings_and_tool_done_keep_their_numbers() {
        use crate::assistants::AssistantSettings;
        assert_eq!(tag(&Request::GetAssistantSettings), 69);
        let set = Request::SetAssistantSettings(AssistantSettings {
            ask_on_phone: true,
            ..Default::default()
        });
        assert_eq!(tag(&set), 70, "SetAssistantSettings moved");
        let reply = Response::AssistantSettings(AssistantSettings::default());
        assert_eq!(tag(&reply), 39, "AssistantSettings moved");
        let done = AssistantEventKind::ToolDone {
            input_hash: [7; 32],
        };
        assert_eq!(tag(&done), 5, "ToolDone moved");
        let ev = Request::AssistantEvent {
            assistant: "claude".into(),
            session: "s".into(),
            event: done,
        };
        let bytes = postcard::to_allocvec(&ev).unwrap();
        assert_eq!(postcard::from_bytes::<Request>(&bytes).unwrap(), ev);
        let bytes = postcard::to_allocvec(&set).unwrap();
        assert_eq!(postcard::from_bytes::<Request>(&bytes).unwrap(), set);
    }
}
