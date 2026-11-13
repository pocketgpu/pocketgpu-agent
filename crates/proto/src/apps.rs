//! `pgpu/apps/1` (F42, board 55): the web apps running on a machine (Jupyter, TensorBoard,
//! ComfyUI, Ollama, …) and a way in to them. One request per bidirectional stream. `List`
//! gets one reply. `Open` gets `Opened`, and from then on the stream carries the raw bytes
//! of one TCP connection to that app on the machine's own `localhost`: the phone or laptop
//! puts a local address in front of it, so a browser reaches the app as if it ran there.
//! Off until `pgpu apps on` at the machine. Seeing what runs needs View; opening one needs
//! Shell, because an app like Jupyter runs any code.

use serde::{Deserialize, Serialize};

use crate::{Error, Permission};

/// The ports looked at for apps, with the app usually found there. Anything else is added
/// at the machine with `pgpu apps add PORT`.
pub const KNOWN: &[(u16, &str)] = &[
    (8888, "Jupyter"),
    (8889, "Jupyter"),
    (8890, "Jupyter"),
    (6006, "TensorBoard"),
    (6007, "TensorBoard"),
    (8188, "ComfyUI"),
    (7860, "Gradio"),
    (7861, "Gradio"),
    (3000, "Open WebUI"),
    (8080, "Open WebUI"),
    (8501, "Streamlit"),
    (11434, "Ollama"),
    (8000, "Web app"),
    (1234, "LM Studio"),
];

/// Ports never opened, even when added: the agent's own and remote logins.
pub const NEVER: &[u16] = &[22, 3389, 5900];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// What runs now, and whether apps are on.
    List,
    /// One connection to the app on this port. Replies `Opened`, then raw bytes.
    Open { port: u16 },
    /// Where to start in the app: `/`, or Jupyter's `/?token=…` so nobody types the token.
    /// Replies `Start`. Agents before it answer with an error: start at `/` then.
    Start { port: u16 },
}

/// What kind of app it is, for its tile and how-to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Jupyter,
    TensorBoard,
    ComfyUI,
    Gradio,
    OpenWebUI,
    Streamlit,
    Ollama,
    LmStudio,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub port: u16,
    /// "Jupyter Lab", or the page's title, or what was given with `pgpu apps add`.
    pub name: String,
    pub kind: Kind,
    /// Added at the machine with `pgpu apps add`, not found by itself.
    pub added: bool,
    /// Turned off at the machine: listed, but `Open` is refused.
    pub off: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Apps {
    /// `pgpu apps on` at the machine. When off, `apps` is empty.
    pub enabled: bool,
    /// Running now, by port.
    pub apps: Vec<App>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Apps(Apps),
    /// The app answered; the stream is raw bytes from here on.
    Opened,
    Error(Error),
    /// The path to open first, starting with `/`.
    Start {
        path: String,
    },
}

impl Request {
    pub fn min_permission(&self) -> Permission {
        match self {
            Self::List => Permission::View,
            Self::Open { .. } | Self::Start { .. } => Permission::Shell,
        }
    }
}

/// The kind an app on `port` is, from the page it answers with (its title, or Ollama's
/// "Ollama is running"), else from the port.
pub fn kind_of(port: u16, page: &str) -> Kind {
    let p = page.to_ascii_lowercase();
    let by_page = [
        ("jupyter", Kind::Jupyter),
        ("tensorboard", Kind::TensorBoard),
        ("comfyui", Kind::ComfyUI),
        ("open webui", Kind::OpenWebUI),
        ("streamlit", Kind::Streamlit),
        ("ollama is running", Kind::Ollama),
        ("gradio", Kind::Gradio),
        ("lm studio", Kind::LmStudio),
    ];
    if let Some((_, k)) = by_page.iter().find(|(w, _)| p.contains(w)) {
        return *k;
    }
    match KNOWN.iter().find(|(k, _)| *k == port).map(|(_, n)| *n) {
        Some("Jupyter") => Kind::Jupyter,
        Some("TensorBoard") => Kind::TensorBoard,
        Some("ComfyUI") => Kind::ComfyUI,
        Some("Gradio") => Kind::Gradio,
        Some("Open WebUI") => Kind::OpenWebUI,
        Some("Streamlit") => Kind::Streamlit,
        Some("Ollama") => Kind::Ollama,
        Some("LM Studio") => Kind::LmStudio,
        _ => Kind::Other,
    }
}

impl Kind {
    /// The name shown when nothing better is known.
    pub fn name(self) -> &'static str {
        match self {
            Self::Jupyter => "Jupyter Lab",
            Self::TensorBoard => "TensorBoard",
            Self::ComfyUI => "ComfyUI",
            Self::Gradio => "Gradio app",
            Self::OpenWebUI => "Open WebUI",
            Self::Streamlit => "Streamlit app",
            Self::Ollama => "Ollama",
            Self::LmStudio => "LM Studio",
            Self::Other => "Web app",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_needs_shell() {
        assert_eq!(Request::List.min_permission(), Permission::View);
        assert_eq!(
            Request::Open { port: 8888 }.min_permission(),
            Permission::Shell
        );
    }

    #[test]
    fn the_page_wins_over_the_port() {
        assert_eq!(kind_of(8888, "<title>JupyterLab</title>"), Kind::Jupyter);
        assert_eq!(kind_of(8000, "<title>JupyterLab</title>"), Kind::Jupyter);
        assert_eq!(kind_of(8080, "Ollama is running"), Kind::Ollama);
        assert_eq!(kind_of(6006, ""), Kind::TensorBoard);
        assert_eq!(kind_of(9999, "<title>My demo</title>"), Kind::Other);
    }

    #[test]
    fn never_lists_no_known_port() {
        assert!(KNOWN.iter().all(|(p, _)| !NEVER.contains(p)));
    }
}
