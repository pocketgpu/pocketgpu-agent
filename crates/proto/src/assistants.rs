//! AI assistants, the rest (F32): approvals asked on the phone, the AI assistants screen and
//! its settings (`specs/F32-assistant-approvals/contracts/approvals.md`).
//!
//! Every enum here is append only: postcard sends a variant as its position.

use serde::{Deserialize, Serialize};

use crate::types::Permission;

/// How long a held action waits for the phone before it ends as timed out.
pub const WAIT_MS: i64 = 300_000;
/// Bytes of the command or tool input the phone sees; a longer one is `truncated`.
pub const DETAIL_MAX: usize = 8 * 1024;
/// Bytes of tool input the hook reads and sends; past this it sends `oversize`.
pub const INPUT_MAX: usize = 1024 * 1024;
/// The owner's own words, at most.
pub const OWN_WORDS_MAX: usize = 20;
/// Characters in one own word.
pub const OWN_WORD_LEN: usize = 64;
/// Requests waiting at once on one computer; past this new ones end as no phone reached.
pub const OPEN_MAX: usize = 64;
/// How long an ended request stays in memory so a late phone sees what happened.
pub const ENDED_KEEP_MS: i64 = 600_000;
/// How long a phone's Allow is remembered for the same session's next desk ask (used once).
pub const REMEMBER_ALLOW_MS: i64 = 60_000;

/// Which hook held the action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AskWhy {
    /// `--ask`: the assistant would ask at the desk; the desk can answer too.
    AskOnPhone,
    /// `--check`: an action the owner wants asked first; only the phone answers.
    AskFirst,
}

/// What kind of action it is (research §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Delete,
    GitPush,
    StopProcess,
    Install,
    /// Matched one of the owner's own words.
    OwnWords,
    /// Nothing recognised (AskOnPhone only).
    Other,
    /// Input over 1 MB or that doesn't parse.
    Unknown,
}

impl Kind {
    /// The kinds the owner can switch on for "Ask me first", as `config.toml` names them.
    pub const ASK_FIRST: [Kind; 4] = [
        Kind::Delete,
        Kind::GitPush,
        Kind::StopProcess,
        Kind::Install,
    ];

    /// The name in `config.toml` and on the command line, for the switchable kinds.
    pub fn config_name(self) -> Option<&'static str> {
        match self {
            Self::Delete => Some("delete"),
            Self::GitPush => Some("git-push"),
            Self::StopProcess => Some("stop"),
            Self::Install => Some("install"),
            Self::OwnWords | Self::Other | Self::Unknown => None,
        }
    }

    pub fn from_config_name(name: &str) -> Option<Kind> {
        Self::ASK_FIRST
            .into_iter()
            .find(|k| k.config_name() == Some(name))
    }
}

/// Why a held request ended without an answer from the phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ended {
    AnsweredAtComputer,
    /// The push reached no Full control phone (none, not entitled, over the limit).
    NoPhone,
    TimedOut,
    /// The hook went away (its socket closed).
    NoLongerNeeded,
}

/// The agent's answer to a hook. A timeout or a failure is never `Allow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// Nothing to do here: the hook prints nothing.
    Pass,
    /// Allowed on a Full control phone.
    Allow,
    Deny {
        message: String,
    },
    NoDecision {
        why: Ended,
    },
}

/// Where a request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApprovalState {
    Waiting,
    Allowed,
    Denied,
    AnsweredAtComputer,
    NoPhone,
    TimedOut,
    NoLongerNeeded,
}

impl ApprovalState {
    pub fn is_open(self) -> bool {
        self == Self::Waiting
    }
}

impl From<Ended> for ApprovalState {
    fn from(e: Ended) -> Self {
        match e {
            Ended::AnsweredAtComputer => Self::AnsweredAtComputer,
            Ended::NoPhone => Self::NoPhone,
            Ended::TimedOut => Self::TimedOut,
            Ended::NoLongerNeeded => Self::NoLongerNeeded,
        }
    }
}

/// A request as the phone sees it (`ListApprovals`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalView {
    pub id: u64,
    pub opened_ms: i64,
    pub ends_ms: i64,
    pub assistant: String,
    /// The session id's first 8 characters.
    pub session_short: String,
    pub why: AskWhy,
    pub kind: Kind,
    pub tool: String,
    /// The first `DETAIL_MAX` bytes of the command or tool input.
    pub detail: String,
    /// The input was longer than `detail`: the phone can only Deny.
    pub truncated: bool,
    pub folder: String,
    pub state: ApprovalState,
    pub answered_by_name: Option<String>,
    /// Every AskFirst request, and AskOnPhone ones whose kind isn't `Other`: the app asks
    /// Face ID before sending Allow.
    pub risky: bool,
}

/// A connected assistant on the AI assistants screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantView {
    /// `claude`, `codex`, `cursor`, `gemini`.
    pub id: String,
    pub name: String,
    pub last_seen_ms: Option<i64>,
    pub alerts: bool,
    pub tools: bool,
    /// Its `--ask` hook is in place.
    pub ask: bool,
    /// Its `--check` hook is in place.
    pub check: bool,
    /// Why "Ask me first" can't work for it, such as Claude Code being too old.
    pub check_blocked: Option<String>,
    pub level: Permission,
}

/// The `[assistants]` settings, read and changed from the phone (Shell).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantSettings {
    pub ask_on_phone: bool,
    /// Only the kinds in `Kind::ASK_FIRST`.
    pub ask_first: Vec<Kind>,
    /// At most `OWN_WORDS_MAX` words of at most `OWN_WORD_LEN` characters.
    pub own_words: Vec<String>,
    /// Per assistant id; one missing means View.
    pub levels: Vec<(String, Permission)>,
}

/// A PocketGPU tool an assistant called through `pgpu mcp --assistant <id>` that needs more
/// than View (F32, US4). The agent checks the assistant's level for each one. Append only:
/// postcard sends the position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCall {
    /// Offers a file to the owner's phones, as `pgpu send` does (Control).
    OfferFile { path: String },
    /// Stops a job this session started (Control).
    StopJob { number: u64 },
    /// Runs a saved action by its name or number (Shell).
    RunAction { name: String },
}

impl ToolCall {
    /// The level an assistant needs for it.
    pub fn needs(&self) -> Permission {
        match self {
            Self::OfferFile { .. } | Self::StopJob { .. } => Permission::Control,
            Self::RunAction { .. } => Permission::Shell,
        }
    }

    /// The MCP tool's name.
    pub fn tool(&self) -> &'static str {
        match self {
            Self::OfferFile { .. } => "offer_file",
            Self::StopJob { .. } => "stop_job",
            Self::RunAction { .. } => "run_action",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag<T: Serialize>(v: &T) -> u8 {
        postcard::to_allocvec(v).unwrap()[0]
    }

    #[test]
    fn variants_keep_their_numbers() {
        assert_eq!(tag(&AskWhy::AskOnPhone), 0);
        assert_eq!(tag(&AskWhy::AskFirst), 1);
        let kinds = [
            Kind::Delete,
            Kind::GitPush,
            Kind::StopProcess,
            Kind::Install,
            Kind::OwnWords,
            Kind::Other,
            Kind::Unknown,
        ];
        for (n, k) in kinds.into_iter().enumerate() {
            assert_eq!(tag(&k) as usize, n, "{k:?} moved");
        }
        let ended = [
            Ended::AnsweredAtComputer,
            Ended::NoPhone,
            Ended::TimedOut,
            Ended::NoLongerNeeded,
        ];
        for (n, e) in ended.into_iter().enumerate() {
            assert_eq!(tag(&e) as usize, n, "{e:?} moved");
        }
        assert_eq!(tag(&Answer::Pass), 0);
        assert_eq!(tag(&Answer::Allow), 1);
        assert_eq!(
            tag(&Answer::Deny {
                message: "m".into()
            }),
            2
        );
        let none = Answer::NoDecision {
            why: Ended::TimedOut,
        };
        assert_eq!(tag(&none), 3);
        let bytes = postcard::to_allocvec(&none).unwrap();
        assert_eq!(postcard::from_bytes::<Answer>(&bytes).unwrap(), none);
        assert_eq!(tag(&ApprovalState::Waiting), 0);
        assert_eq!(tag(&ApprovalState::NoLongerNeeded), 6);
    }

    #[test]
    fn config_names_round_trip() {
        for k in Kind::ASK_FIRST {
            let name = k.config_name().unwrap();
            assert_eq!(Kind::from_config_name(name), Some(k));
        }
        assert_eq!(Kind::from_config_name("other"), None);
        assert_eq!(Kind::OwnWords.config_name(), None);
    }

    #[test]
    fn ended_maps_to_its_state() {
        assert_eq!(ApprovalState::from(Ended::NoPhone), ApprovalState::NoPhone);
        assert!(ApprovalState::Waiting.is_open());
        assert!(!ApprovalState::TimedOut.is_open());
    }

    #[test]
    fn settings_round_trip() {
        let s = AssistantSettings {
            ask_on_phone: true,
            ask_first: vec![Kind::Delete, Kind::GitPush],
            own_words: vec!["terraform apply".into()],
            levels: vec![("claude".into(), Permission::View)],
        };
        let bytes = postcard::to_allocvec(&s).unwrap();
        assert_eq!(
            postcard::from_bytes::<AssistantSettings>(&bytes).unwrap(),
            s
        );
    }
}
