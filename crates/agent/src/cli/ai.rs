//! `pgpu ai [status | add PORT [NAME] | remove PORT]`: the model servers besides Ollama that
//! phones can chat with (F18, board 86). LM Studio, llama.cpp, vLLM and Jan are found by
//! themselves; another port is added here.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::ai::ModelServer;
use pocketgpu_proto::local::{Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show the model servers here and what they serve (the default).
    Status,
    /// Look for a model server on another port too: pgpu ai add 5000 "My server"
    Add {
        /// Its port, or its address on this computer such as http://127.0.0.1:5000.
        address: String,
        /// The name phones show; else the server's own, or its port.
        name: Option<String>,
    },
    /// Stop looking on a port added with `pgpu ai add`.
    Remove { port: u16 },
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let req = match action.unwrap_or(Action::Status) {
        Action::Status => Request::GetModelServers,
        Action::Add { address, name } => Request::AddModelServer {
            port: crate::ai::port_of(&address).map_err(anyhow::Error::msg)?,
            name,
        },
        Action::Remove { port } => Request::RemoveModelServer { port },
    };
    let servers = match client::call(paths, req).await? {
        Response::ModelServers(s) => s,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::to_string(&servers)?);
    } else {
        print!("{}", render(&servers));
    }
    Ok(())
}

pub fn render(servers: &[ModelServer]) -> String {
    let mut out = String::from(
        "Model servers besides Ollama (`pgpu status` shows Ollama). Your phones with Control can\nchat with what's running:\n\n",
    );
    for s in servers {
        let what = if !s.up {
            "not running".to_string()
        } else if s.models.is_empty() {
            "running, no model loaded".to_string()
        } else {
            let names: Vec<&str> = s.models.iter().map(|m| m.id.as_str()).collect();
            format!("running: {}", names.join(", "))
        };
        let added = if s.added { "  (added)" } else { "" };
        out.push_str(&format!("  {:<6} {:<10} {what}{added}\n", s.port, s.name));
    }
    out.push_str("\nAnother one? pgpu ai add 5000\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketgpu_proto::ai::ServedModel;

    #[test]
    fn lists_each_server_with_what_it_serves() {
        let r = render(&[
            ModelServer {
                name: "LM Studio".into(),
                port: 1234,
                added: false,
                up: true,
                models: vec![ServedModel {
                    id: "qwen2.5-7b-instruct".into(),
                    context: None,
                }],
            },
            ModelServer {
                name: "Port 5000".into(),
                port: 5000,
                added: true,
                up: false,
                models: vec![],
            },
        ]);
        assert!(
            r.contains("1234   LM Studio  running: qwen2.5-7b-instruct"),
            "{r}"
        );
        assert!(r.contains("5000   Port 5000  not running  (added)"), "{r}");
        assert!(r.contains("pgpu ai add 5000"));
    }
}
