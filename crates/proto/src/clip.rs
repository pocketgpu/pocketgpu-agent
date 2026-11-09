//! `pgpu/clip/1` (F41): text to and from a machine's clipboard. One request per
//! bidirectional stream, one reply each. Off until `pgpu clip on` at the machine. Putting
//! text needs Control; reading needs Shell, because clipboards hold passwords.

use serde::{Deserialize, Serialize};

use crate::{Error, Permission};

/// Longest text, in bytes.
pub const MAX_TEXT: usize = 256 * 1024;
/// The in-memory clipboard forgets its text after this long.
pub const CLEARS_AFTER_S: u32 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Whether the clipboard is on and where text goes.
    Info,
    /// `secret` asks clipboard history and cloud sync to skip the text (Windows, Mac);
    /// `once` clears the in-memory clipboard after its first read.
    Put {
        text: String,
        secret: bool,
        once: bool,
    },
    Get,
}

/// Where text goes on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Place {
    /// The desktop's own clipboard: the agent runs in a session with a screen.
    System,
    /// PocketGPU's own clipboard in the agent's memory, never written to disk.
    Memory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub enabled: bool,
    pub place: Place,
    /// How long the in-memory clipboard keeps text; `None` for the system one.
    pub clears_after_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Info(Info),
    /// `Put` worked.
    Ok,
    /// `age_ms`: how long ago it was put there, when the agent knows.
    Text {
        text: String,
        age_ms: Option<u64>,
    },
    /// Nothing to read, or no text.
    Empty,
    /// The system marks what is there as secret (a password manager), so it isn't sent.
    Secret,
    Error(Error),
}

impl Request {
    pub fn min_permission(&self) -> Permission {
        match self {
            Self::Info => Permission::View,
            Self::Put { .. } => Permission::Control,
            Self::Get => Permission::Shell,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_needs_more_than_putting() {
        let put = Request::Put {
            text: "x".into(),
            secret: false,
            once: false,
        };
        assert_eq!(Request::Info.min_permission(), Permission::View);
        assert_eq!(put.min_permission(), Permission::Control);
        assert_eq!(Request::Get.min_permission(), Permission::Shell);
    }

    #[test]
    fn the_longest_text_fits_in_a_frame() {
        let put = Request::Put {
            text: "é".repeat(MAX_TEXT / 2),
            secret: true,
            once: true,
        };
        assert!(crate::frame::encode(&put).is_ok());
    }
}
