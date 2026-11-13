//! Jupyter without typing its token (F42): each running Jupyter server writes a small file
//! into its runtime folder (`jpserver-<pid>.json`, `nbserver-<pid>.json` for the classic
//! Notebook) with its port, base URL and token. The start address for a device that opens it
//! is `<base_url>?token=<token>`: Jupyter then keeps the login in a cookie. Only a device
//! allowed to open apps (Shell) gets it; such a device could read the file anyway.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Server {
    port: u16,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    token: String,
}

/// Where Jupyter keeps its runtime files, as `jupyter --runtime-dir` says.
pub fn runtime_dirs() -> Vec<PathBuf> {
    if let Some(d) = std::env::var_os("JUPYTER_RUNTIME_DIR") {
        return vec![PathBuf::from(d)];
    }
    let mut data = Vec::new();
    if let Some(d) = std::env::var_os("JUPYTER_DATA_DIR") {
        data.push(PathBuf::from(d));
    } else if let Some(base) = directories::BaseDirs::new() {
        if cfg!(target_os = "macos") {
            data.push(base.home_dir().join("Library/Jupyter"));
        } else {
            // Linux: $XDG_DATA_HOME/jupyter; Windows: %APPDATA%\jupyter.
            data.push(base.data_dir().join("jupyter"));
        }
    }
    data.into_iter().map(|d| d.join("runtime")).collect()
}

/// The start path for the Jupyter server on `port`: `/?token=…`, or `None` when no server
/// file says that port or it has no token (a password, or none at all).
pub fn start_path(port: u16) -> Option<String> {
    runtime_dirs()
        .iter()
        .find_map(|dir| start_path_in(dir, port))
}

fn start_path_in(dir: &Path, port: u16) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let server_file = (name.starts_with("jpserver-") || name.starts_with("nbserver-"))
            && name.ends_with(".json");
        if !server_file {
            return None;
        }
        let text = std::fs::read_to_string(e.path()).ok()?;
        let s: Server = serde_json::from_str(&text).ok()?;
        (s.port == port && !s.token.is_empty()).then(|| path_for(&s))
    })
}

fn path_for(s: &Server) -> String {
    let mut base = s.base_url.trim().to_string();
    if !base.starts_with('/') {
        base.insert(0, '/');
    }
    if !base.ends_with('/') {
        base.push('/');
    }
    // Tokens are hex; anything else is left out rather than trusted into a URL.
    let token: String = s
        .token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!("{base}?token={token}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, port: u16, token: &str, base: &str) {
        let body = serde_json::json!({
            "port": port, "token": token, "base_url": base, "hostname": "localhost", "pid": 1
        });
        std::fs::write(dir.join(name), body.to_string()).unwrap();
    }

    #[test]
    fn finds_the_server_on_that_port() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "jpserver-11.json", 8888, "abc123", "/");
        write(d.path(), "nbserver-12.json", 8890, "def456", "/nb");
        std::fs::write(d.path().join("kernel-1.json"), "{}").unwrap();
        assert_eq!(
            start_path_in(d.path(), 8888).as_deref(),
            Some("/?token=abc123")
        );
        assert_eq!(
            start_path_in(d.path(), 8890).as_deref(),
            Some("/nb/?token=def456")
        );
        assert_eq!(start_path_in(d.path(), 6006), None);
    }

    #[test]
    fn no_token_no_path_and_odd_tokens_are_cleaned() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "jpserver-1.json", 8888, "", "/");
        assert_eq!(start_path_in(d.path(), 8888), None);
        write(d.path(), "jpserver-2.json", 8889, "a&b=c", "/");
        assert_eq!(
            start_path_in(d.path(), 8889).as_deref(),
            Some("/?token=abc")
        );
    }
}
