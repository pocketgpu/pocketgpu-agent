//! `pgpu/clip/1`: text to and from a machine's clipboard (F41).

use pocketgpu_proto::alpn;
use pocketgpu_proto::clip::{Info, Request, Response};

use crate::{ClientError, Machine, request};

/// What a machine's clipboard holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipContent {
    /// `age_ms`: how long ago it was put there, when the machine knows.
    Text {
        text: String,
        age_ms: Option<u64>,
    },
    Empty,
    /// The machine's system marks it as secret (a password manager), so it wasn't sent.
    Secret,
}

impl Machine {
    /// Whether the clipboard is on and where text goes there.
    pub async fn clip_info(&self) -> Result<Info, ClientError> {
        match self.clip(&Request::Info).await? {
            Response::Info(info) => Ok(info),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Puts text on the machine's clipboard (needs Control). `secret` keeps it out of
    /// clipboard history and cloud sync there; `once` clears the in-memory clipboard after
    /// its first read.
    pub async fn clip_put(&self, text: &str, secret: bool, once: bool) -> Result<(), ClientError> {
        let req = Request::Put {
            text: text.into(),
            secret,
            once,
        };
        match self.clip(&req).await? {
            Response::Ok => Ok(()),
            _ => Err(ClientError::NoReply),
        }
    }

    /// What is on the machine's clipboard (needs Shell).
    pub async fn clip_get(&self) -> Result<ClipContent, ClientError> {
        match self.clip(&Request::Get).await? {
            Response::Text { text, age_ms } => Ok(ClipContent::Text { text, age_ms }),
            Response::Empty => Ok(ClipContent::Empty),
            Response::Secret => Ok(ClipContent::Secret),
            _ => Err(ClientError::NoReply),
        }
    }

    async fn clip(&self, req: &Request) -> Result<Response, ClientError> {
        let conn = self.clip.get(self, alpn::CLIP).await?;
        match request(&conn, req).await? {
            Response::Error(e) => Err(ClientError::Remote(e)),
            other => Ok(other),
        }
    }
}
