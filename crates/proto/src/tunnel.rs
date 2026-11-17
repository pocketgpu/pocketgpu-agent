//! `pgpu/tunnel/1`: one stream per request to the machine's Ollama or another model server on
//! it (F17, F18).
//!
//! The phone sends one `Request` (Ollama) or `Server` (LM Studio, llama.cpp, vLLM, …); the agent
//! forwards it on the machine if the path is on its allowlist and the phone has the permission
//! it needs, then streams the reply: `Head`, any number of `Chunk`s, `End`. `Servers` asks which
//! model servers there are, answered the same way with one JSON body. Closing the stream
//! cancels the request, which stops a pull or a reply. The servers never listen on the
//! network for this. An agent older than `Server` closes the stream without a reply.

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
    /// A request to another model server on the machine, by its port (`ai::KNOWN`, or added
    /// at the machine): only the calls `server_permission_for` names.
    Server {
        port: u16,
        method: Method,
        path: String,
        body: Vec<u8>,
    },
    /// The model servers on the machine and their models: `ai::ModelServer`s as JSON. View.
    Servers,
    /// Sets up Ollama on the machine as its user (`start`, Shell), or asks how far it is
    /// (View): `ai::OllamaSetup` as JSON (board 87).
    SetUpOllama { start: bool },
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

/// The calls forwarded to another model server (F18): listing its models is open to every
/// paired phone, chatting needs Control, like Ollama's.
pub fn server_permission_for(method: Method, path: &str) -> Option<Permission> {
    match (method, path) {
        (Method::Get, "/v1/models") => Some(Permission::View),
        (Method::Post, "/v1/chat/completions") => Some(Permission::Control),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listing_and_chatting_reach_another_server() {
        assert_eq!(
            server_permission_for(Method::Get, "/v1/models"),
            Some(Permission::View)
        );
        assert_eq!(
            server_permission_for(Method::Post, "/v1/chat/completions"),
            Some(Permission::Control)
        );
        assert_eq!(server_permission_for(Method::Post, "/v1/completions"), None);
        assert_eq!(
            server_permission_for(Method::Get, "/v1/models/../admin"),
            None
        );
        assert_eq!(server_permission_for(Method::Delete, "/v1/models"), None);
    }

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
