//! `pgpu/docker/1` (F28): the machine's Docker containers. One request per bidirectional
//! stream. `List` and `Act` get one reply; `Logs` gets `Log` frames until the container
//! stops or the app closes the stream. Off until `pgpu docker on` at the machine, because
//! Docker access is root access.

use serde::{Deserialize, Serialize};

use crate::{Error, Permission};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Every container, running ones first.
    List,
    Act {
        id: String,
        action: Action,
    },
    /// The last `tail` lines, then new ones as they come.
    Logs {
        id: String,
        tail: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Container {
    /// The short ID (12 hex characters).
    pub id: String,
    pub name: String,
    pub image: String,
    /// Docker's state: `running`, `exited`, `paused`, `restarting`, `created` or `dead`.
    pub state: String,
    /// Docker's own words, such as "Up 3 hours" or "Exited (0) 2 minutes ago".
    pub status: String,
    /// Unix milliseconds.
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    pub stderr: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Containers(Vec<Container>),
    /// `Act` worked.
    Ok,
    /// Some lines of a log; more follow while it runs.
    Log(Vec<LogLine>),
    Error(Error),
}

impl Request {
    pub fn min_permission(&self) -> Permission {
        match self {
            Self::List | Self::Logs { .. } => Permission::View,
            Self::Act { .. } => Permission::Control,
        }
    }
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// Container IDs and names as Docker writes them: nothing that could change the API path.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        && !id.starts_with(['.', '-'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_cannot_leave_the_containers_path() {
        assert!(valid_id("ba916de6c8ed"));
        assert!(valid_id("pgpu-demo"));
        assert!(!valid_id("../images"));
        assert!(!valid_id("x/start"));
        assert!(!valid_id("x?all=1"));
        assert!(!valid_id(""));
        assert!(!valid_id(".hidden"));
    }
}
