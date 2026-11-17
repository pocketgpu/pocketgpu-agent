//! Model servers besides Ollama (F18, board 86): LM Studio, llama.cpp, vLLM, Jan, or a port
//! added with `pgpu ai add`. Each is asked for its models at the machine's own 127.0.0.1, all
//! at once and briefly; phones chat with them through the tunnel (tunnel/mod.rs), and only on
//! these ports.

use std::sync::Arc;
use std::time::Duration;

use pocketgpu_proto::ai::{KNOWN, ModelServer, ServedModel, name_for};
use pocketgpu_proto::local::{Request, Response};
use serde::Deserialize;

use crate::daemon::state::Agent;
use crate::store::config::{AddedApp, Config};

/// How long a server has to say what it serves.
const ASK: Duration = Duration::from_millis(1500);

fn config(agent: &Agent) -> Config {
    agent.config.read().expect("config lock poisoned").clone()
}

/// Where a model server may be: the known ports, then the added ones, each once.
fn places(cfg: &Config) -> Vec<(u16, Option<String>, bool)> {
    let mut out: Vec<(u16, Option<String>, bool)> =
        KNOWN.iter().map(|(p, _)| (*p, None, false)).collect();
    for a in &cfg.ai_added {
        match out.iter_mut().find(|(p, ..)| *p == a.port) {
            Some(place) => place.1 = a.name.clone(),
            None => out.push((a.port, a.name.clone(), true)),
        }
    }
    out
}

/// Whether a phone may reach a model server on this port.
pub fn allowed(agent: &Agent, port: u16) -> bool {
    places(&config(agent)).iter().any(|(p, ..)| *p == port)
}

/// What to call the server on `port`, for messages before it has answered.
pub fn name(agent: &Agent, port: u16) -> String {
    let given = config(agent)
        .ai_added
        .into_iter()
        .find(|a| a.port == port)
        .and_then(|a| a.name);
    given
        .or_else(|| name_for(port, None).map(str::to_string))
        .unwrap_or_else(|| format!("The server on port {port}"))
}

/// The client for the machine's own servers: no proxy, no redirect elsewhere.
pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(400))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("a plain HTTP client")
}

/// Every model server, asked at once. One that doesn't answer, or not as an OpenAI-style
/// server would, is down.
pub async fn servers(agent: &Agent) -> Vec<ModelServer> {
    let http = http();
    let mut asks = tokio::task::JoinSet::new();
    for (i, (port, given, added)) in places(&config(agent)).into_iter().enumerate() {
        let http = http.clone();
        asks.spawn(async move { (i, ask(&http, port, given, added).await) });
    }
    let mut out: Vec<(usize, ModelServer)> = asks.join_all().await;
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, s)| s).collect()
}

async fn ask(http: &reqwest::Client, port: u16, given: Option<String>, added: bool) -> ModelServer {
    let url = format!("http://127.0.0.1:{port}/v1/models");
    let answer = async {
        let res = http.get(&url).timeout(ASK).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        read_models(&res.bytes().await.ok()?)
    };
    let (up, models, owned_by) = match answer.await {
        Some((models, owned_by)) => (true, models, owned_by),
        None => (false, Vec::new(), None),
    };
    let name = given
        .or_else(|| name_for(port, owned_by.as_deref()).map(str::to_string))
        .unwrap_or_else(|| format!("Port {port}"));
    ModelServer {
        name,
        port,
        added,
        up,
        models,
    }
}

#[derive(Deserialize)]
struct List {
    data: Vec<Listed>,
}

#[derive(Deserialize)]
struct Listed {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
    #[serde(default)]
    max_model_len: Option<u32>,
}

/// An OpenAI-style model list: the models, and who the first says owns it.
fn read_models(body: &[u8]) -> Option<(Vec<ServedModel>, Option<String>)> {
    let list: List = serde_json::from_slice(body).ok()?;
    let owned_by = list.data.first().and_then(|m| m.owned_by.clone());
    let models = list
        .data
        .into_iter()
        .filter(|m| !m.id.trim().is_empty())
        .map(|m| ServedModel {
            id: m.id,
            context: m.max_model_len,
        })
        .collect();
    Some((models, owned_by))
}

/// `5000`, `127.0.0.1:5000` or `http://localhost:5000` → 5000. Only this machine.
pub fn port_of(text: &str) -> Result<u16, String> {
    let rest = text.trim().trim_end_matches('/');
    let rest = rest
        .strip_prefix("http://")
        .or_else(|| rest.strip_prefix("https://"))
        .unwrap_or(rest);
    let port = match rest.rsplit_once(':') {
        Some((host, port)) => {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
                return Err(format!(
                    "{host} isn't this computer: PocketGPU only talks to model servers here"
                ));
            }
            port
        }
        None => rest,
    };
    match port.parse::<u16>() {
        Ok(p) if p != 0 => Ok(p),
        _ => Err(format!("{text} isn't a port such as 5000")),
    }
}

/// The command line's requests (`pgpu ai`).
pub async fn local_request(agent: &Arc<Agent>, req: Request) -> Response {
    let done = match req {
        Request::GetModelServers => Ok(()),
        Request::AddModelServer { port, name } => {
            if port == 0 {
                Err("port 0 isn't a port".to_string())
            } else {
                let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
                change(agent, |c| {
                    c.ai_added.retain(|a| a.port != port);
                    c.ai_added.push(AddedApp { port, name });
                    c.ai_added.sort_by_key(|a| a.port);
                })
            }
        }
        Request::RemoveModelServer { port } => {
            if config(agent).ai_added.iter().any(|a| a.port == port) {
                change(agent, |c| c.ai_added.retain(|a| a.port != port))
            } else {
                Err(format!("port {port} wasn't added with: pgpu ai add"))
            }
        }
        _ => Err("not a model server request".into()),
    };
    match done {
        Ok(()) => Response::ModelServers(servers(agent).await),
        Err(e) => Response::Error(e),
    }
}

fn change(agent: &Agent, f: impl FnOnce(&mut Config)) -> Result<(), String> {
    let mut cfg = agent.config.write().expect("config lock poisoned");
    let mut next = cfg.clone();
    f(&mut next);
    next.save(&agent.paths.config_file())
        .map(|()| *cfg = next)
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_openai_model_list() {
        let body = br#"{"object":"list","data":[{"id":"Qwen/Qwen2.5-7B","object":"model","owned_by":"vllm","max_model_len":32768},{"id":""}]}"#;
        let (models, owned_by) = read_models(body).unwrap();
        assert_eq!(
            models,
            vec![ServedModel {
                id: "Qwen/Qwen2.5-7B".into(),
                context: Some(32768)
            }]
        );
        assert_eq!(owned_by.as_deref(), Some("vllm"));
        assert!(read_models(b"<html>Open WebUI</html>").is_none());
        assert!(read_models(br#"{"error":"unauthorized"}"#).is_none());
    }

    #[test]
    fn takes_only_ports_on_this_machine() {
        assert_eq!(port_of("5000"), Ok(5000));
        assert_eq!(port_of("http://127.0.0.1:5000/"), Ok(5000));
        assert_eq!(port_of("localhost:8081"), Ok(8081));
        assert_eq!(port_of("http://[::1]:9000"), Ok(9000));
        assert!(port_of("http://10.0.0.5:5000").is_err());
        assert!(port_of("example.com:80").is_err());
        assert!(port_of("0").is_err());
        assert!(port_of("five").is_err());
    }

    #[test]
    fn an_added_port_joins_the_known_ones_once() {
        let cfg = Config {
            ai_added: vec![
                AddedApp {
                    port: 8080,
                    name: Some("My llama".into()),
                },
                AddedApp {
                    port: 5000,
                    name: None,
                },
            ],
            ..Config::default()
        };
        let places = places(&cfg);
        assert_eq!(places.len(), KNOWN.len() + 1);
        assert!(places.contains(&(8080, Some("My llama".into()), false)));
        assert!(places.contains(&(5000, None, true)));
    }

    #[tokio::test]
    async fn a_server_that_answers_is_up_with_its_models() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await;
            let body = r#"{"data":[{"id":"qwen2.5-7b-instruct","owned_by":"organization_owner"}]}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(reply.as_bytes()).await.unwrap();
        });
        let up = ask(&http(), port, Some("Mine".into()), true).await;
        assert!(up.up);
        assert_eq!(up.name, "Mine");
        assert_eq!(up.models[0].id, "qwen2.5-7b-instruct");

        let down = ask(&http(), port, None, true).await;
        assert!(!down.up);
        assert_eq!(down.name, format!("Port {port}"));
    }
}
