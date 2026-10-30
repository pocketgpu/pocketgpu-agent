//! `pgpu/shell/1`: one bidirectional stream per terminal (F16).
//!
//! The phone opens a stream and sends `Open` (or `List` alone). The agent answers `Opened`,
//! then both sides stream: `Input` and `Resize` one way, `Output` and `Title` the other. A
//! stream that ends without `Close` detaches the session, which the phone can reattach to
//! from the same key for 10 minutes. `Tools` alone asks which launchable programs exist.

use serde::{Deserialize, Serialize};

use crate::types::Error;

/// Sessions one phone may keep on a machine.
pub const MAX_SESSIONS_PER_PHONE: usize = 3;
/// How long a detached session waits for its phone.
pub const DETACH_TIMEOUT_S: u64 = 10 * 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToAgent {
    /// A new session, or reattach to `session`. Terminal size in character cells.
    Open {
        cols: u16,
        rows: u16,
        session: Option<u64>,
    },
    /// This phone's sessions on the machine; the agent replies `Sessions` and ends the stream.
    List,
    /// Keystrokes and pasted text, as bytes for the terminal.
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// End the session: the shell is killed.
    Close,
    /// Which of these programs the user's shell finds (the terminal's tool launchers); the
    /// agent replies `Tools` and ends the stream. At most [`MAX_TOOL_NAMES`] plain names.
    Tools(Vec<String>),
}

/// Program names one `Tools` request may ask about.
pub const MAX_TOOL_NAMES: usize = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToPhone {
    Opened {
        session: u64,
        /// What runs in the foreground, such as `bash` or `htop`.
        title: String,
        /// A reattach: the recent output follows as one `Output`.
        reattached: bool,
    },
    Output(Vec<u8>),
    /// The foreground program changed.
    Title(String),
    /// The shell ended; the stream closes after this.
    Exited {
        code: Option<i32>,
    },
    Sessions(Vec<SessionInfo>),
    Error(Error),
    /// The asked-for programs that are installed, in the order asked.
    Tools(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session: u64,
    pub title: String,
    pub started_ms: i64,
    /// Another stream of this phone is attached right now.
    pub attached: bool,
}
