//! `pgpu mcp`: an MCP server on stdin and stdout for AI agents on this machine (F32, first
//! step). Claude Code, Codex, Gemini CLI and others start it themselves and get the same
//! picture as the phone: GPU status, jobs and their output, plus a tool that sends a note to
//! the paired phones ("tell me when you're done"). Read-only apart from that note; it reaches
//! the agent the way `pgpu` does, as the same user.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use pocketgpu_proto::local::{Request, Response};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::ipc::client;
use crate::store::paths::Paths;

/// Protocol versions this server speaks; it answers with the client's when it knows it.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// Notes per hour from one session, so a looping agent can't flood the phone.
const NOTES_PER_HOUR: usize = 10;
/// The most output `job_output` reads from the end of a log.
const TAIL_BYTES: u64 = 64 * 1024;

const INSTRUCTIONS: &str = "PocketGPU shows this machine's GPUs and jobs on the user's phone. \
Use machine_status before starting GPU work, list_jobs and job_output to follow jobs started \
with `pgpu run -- <command>`, and notify_phone once when a long task finishes or needs the \
user. To get an alert when a command ends, run it as `pgpu run -- <command>`.";

/// How the server reaches the agent; a fake in tests.
pub(crate) trait Agent {
    async fn call(&self, req: Request) -> Result<Response>;
}

struct Local<'a>(&'a Paths);

impl Agent for Local<'_> {
    async fn call(&self, req: Request) -> Result<Response> {
        client::call(self.0, req).await
    }
}

pub async fn run(paths: &Paths) -> Result<()> {
    let mut server = Server::new(Local(paths));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle(&line).await {
            stdout.write_all(reply.to_string().as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

pub(crate) struct Server<A> {
    agent: A,
    notes: Vec<Instant>,
}

impl<A: Agent> Server<A> {
    pub(crate) fn new(agent: A) -> Self {
        Self {
            agent,
            notes: Vec::new(),
        }
    }

    /// The reply to one JSON-RPC message; `None` for notifications.
    pub(crate) async fn handle(&mut self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => return Some(error(Value::Null, -32700, &format!("not JSON: {e}"))),
        };
        let id = msg.get("id").cloned()?;
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked
                    .filter(|v| VERSIONS.contains(v))
                    .unwrap_or(VERSIONS[0]);
                result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "pocketgpu", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let content = match self.call_tool(name, &args).await {
                    Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                    Err(e) => json!({
                        "content": [{ "type": "text", "text": format!("{e:#}") }],
                        "isError": true,
                    }),
                };
                result(id, content)
            }
            other => error(id, -32601, &format!("unknown method {other}")),
        })
    }

    async fn call_tool(&mut self, name: &str, args: &Value) -> Result<String> {
        match name {
            "machine_status" => match self.agent.call(Request::Status).await? {
                Response::Status(s) => pretty(&super::status::to_json(&s)),
                other => unexpected(other),
            },
            "list_jobs" => {
                let limit = arg_u64(args, "limit").unwrap_or(10).clamp(1, 50) as u32;
                match self.agent.call(Request::ListJobs { limit }).await? {
                    Response::Jobs(jobs) => pretty(&jobs),
                    other => unexpected(other),
                }
            }
            "job_output" => {
                let Some(id) = arg_u64(args, "job") else {
                    bail!("give the job number, from list_jobs");
                };
                let lines = arg_u64(args, "lines").unwrap_or(50).clamp(1, 500) as usize;
                match self.agent.call(Request::GetJob { id }).await? {
                    Response::Job { detail, log_path } => {
                        let output = tail(std::path::Path::new(&log_path), lines);
                        let state = super::jobs::state_label(detail.summary.state);
                        Ok(format!("Job {id} is {state}. Last lines:\n{output}"))
                    }
                    other => unexpected(other),
                }
            }
            "notify_phone" => {
                let title = args.get("title").and_then(Value::as_str).unwrap_or("");
                let Some(message) = args.get("message").and_then(Value::as_str) else {
                    bail!("give the message to send");
                };
                let hour_ago = Instant::now() - Duration::from_secs(3600);
                self.notes.retain(|t| *t > hour_ago);
                if self.notes.len() >= NOTES_PER_HOUR {
                    bail!("at most {NOTES_PER_HOUR} notes an hour; send one when the task ends");
                }
                let req = Request::Notify {
                    title: title.into(),
                    body: message.into(),
                };
                match self.agent.call(req).await? {
                    Response::Notified { held: true, .. } => {
                        Ok("Not sent: the user has paused alerts or turned job alerts off.".into())
                    }
                    Response::Notified {
                        phones: 0,
                        channels: 0,
                        ..
                    } => Ok(
                        "Not delivered: no phone with alerts on is paired and no alert channel \
                         is set up. The user can run `pgpu pair` or `pgpu notify add`."
                            .into(),
                    ),
                    Response::Notified {
                        phones, channels, ..
                    } => {
                        self.notes.push(Instant::now());
                        Ok(format!(
                            "Sent to {phones} phone(s) and {channels} other alert channel(s)."
                        ))
                    }
                    other => unexpected(other),
                }
            }
            other => bail!("no tool named {other}"),
        }
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "machine_status",
            "description": "This machine's GPUs (load, memory, temperature, power), CPU, RAM and disk, and how many phones are paired and connected.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "list_jobs",
            "description": "Recent jobs started with `pgpu run -- <command>`: number, name, state (running, finished, crashed, stopped), start and end time and exit code.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "How many, newest first (default 10)." },
                },
            },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "job_output",
            "description": "The last lines a job printed, and whether it is still running.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "job": { "type": "integer", "description": "The job number from list_jobs." },
                    "lines": { "type": "integer", "minimum": 1, "maximum": 500, "description": "How many lines from the end (default 50)." },
                },
                "required": ["job"],
            },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "notify_phone",
            "description": "Send a short note to the user's phone, like a job alert. Use it once when a long task finishes, fails, or needs the user; not for progress updates. At most 10 an hour.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Up to 60 characters, e.g. 'Fine-tune finished'." },
                    "message": { "type": "string", "description": "Up to 140 characters: what happened and what's next." },
                },
                "required": ["message"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true },
        },
    ])
}

fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn pretty<T: serde::Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(v)?)
}

fn unexpected(r: Response) -> Result<String> {
    match r {
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
}

/// The last `lines` lines of the file, reading at most its last 64 KB.
fn tail(path: &std::path::Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return "(no output yet)".into();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    let out = all[all.len().saturating_sub(lines)..].join("\n");
    if out.is_empty() {
        "(no output yet)".into()
    } else {
        out
    }
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
