//! Model servers besides Ollama (F18, board 86): LM Studio, llama.cpp, vLLM, Jan, or one added
//! at the machine with `pgpu ai add`. They all speak OpenAI's chat language. A phone lists them
//! with `tunnel::ToAgent::Servers` and chats through `tunnel::ToAgent::Server`; the agent only
//! ever talks to them on the machine's own 127.0.0.1, and only on these ports or added ones.

use serde::{Deserialize, Serialize};

/// The ports looked at for a model server, with the one usually found there.
pub const KNOWN: &[(u16, &str)] = &[
    (1234, "LM Studio"),
    (8080, "llama.cpp"),
    (8000, "vLLM"),
    (1337, "Jan"),
];

/// A model server on the machine, found or added, and the models it serves now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelServer {
    pub name: String,
    pub port: u16,
    /// Added with `pgpu ai add`, not one of `KNOWN`.
    pub added: bool,
    /// It answered just now.
    pub up: bool,
    pub models: Vec<ServedModel>,
}

/// A model a server can chat with now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedModel {
    pub id: String,
    /// How many tokens it holds, when the server says (vLLM's `max_model_len`).
    pub context: Option<u32>,
}

/// Setting up Ollama from a phone (board 87.2), as the agent reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OllamaSetup {
    pub phase: SetupPhase,
    /// Bytes downloaded, of `total` (0 until the size is known).
    pub done: u64,
    pub total: u64,
    /// Why it stopped, in `Failed`.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPhase {
    /// Not asked for since the agent started.
    #[default]
    Idle,
    Download,
    Unpack,
    Start,
    Ready,
    Failed,
}

/// What to call the server on `port`: what its models say about it (`owned_by`), else the
/// one usually found on that port.
pub fn name_for(port: u16, owned_by: Option<&str>) -> Option<&'static str> {
    match owned_by.map(str::to_ascii_lowercase).as_deref() {
        Some("vllm") => Some("vLLM"),
        Some("llamacpp" | "llama.cpp") => Some("llama.cpp"),
        _ => KNOWN.iter().find(|(p, _)| *p == port).map(|(_, n)| *n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_server_by_what_it_says_then_by_its_port() {
        assert_eq!(name_for(8000, Some("vllm")), Some("vLLM"));
        assert_eq!(name_for(1234, Some("llamacpp")), Some("llama.cpp"));
        assert_eq!(
            name_for(1234, Some("organization_owner")),
            Some("LM Studio")
        );
        assert_eq!(name_for(1337, None), Some("Jan"));
        assert_eq!(name_for(5000, None), None);
    }
}
