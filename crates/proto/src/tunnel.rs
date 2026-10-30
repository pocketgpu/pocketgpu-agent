//! `pgpu/tunnel/1`: one stream per request to the machine's Ollama (F17, F18).
//!
//! The phone sends one `Request`; the agent forwards it to Ollama on the machine if the path
//! is on its allowlist and the phone has the permission it needs, then streams the reply:
//! `Head`, any number of `Chunk`s, `End`. Closing the stream cancels the request, which stops
//! a pull. Ollama itself never listens on the network.

use serde::{Deserialize, Serialize};

use crate::types::{Error, Permission};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    Get,
    Post,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToAgent {
    /// `path` is Ollama's, such as `/api/tags`; `body` is its JSON.
    Request {
        method: Method,
        path: String,
        body: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToPhone {
    /// Ollama's HTTP status.
    Head {
        status: u16,
    },
    /// Part of the body; streamed replies such as pull progress arrive as many chunks.
    Chunk(Vec<u8>),
    End,
    Error(Error),
}

/// The Ollama calls the agent forwards and the permission each needs (F17): reading is open
/// to every paired phone, loading and chatting need Control, downloading and deleting Shell.
pub fn permission_for(method: Method, path: &str) -> Option<Permission> {
    use Method::*;
    match (method, path) {
        (Get, "/api/version" | "/api/tags" | "/api/ps") | (Post, "/api/show") => {
            Some(Permission::View)
        }
        (Post, "/api/generate" | "/api/chat") => Some(Permission::Control),
        (Post, "/api/pull") | (Delete, "/api/delete") => Some(Permission::Shell),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_calls_are_forwarded() {
        assert_eq!(
            permission_for(Method::Get, "/api/tags"),
            Some(Permission::View)
        );
        assert_eq!(
            permission_for(Method::Post, "/api/chat"),
            Some(Permission::Control)
        );
        assert_eq!(
            permission_for(Method::Delete, "/api/delete"),
            Some(Permission::Shell)
        );
        assert_eq!(permission_for(Method::Post, "/api/create"), None);
        assert_eq!(permission_for(Method::Get, "/api/chat"), None);
        assert_eq!(permission_for(Method::Get, "/api/tags/../../etc"), None);
    }
}
