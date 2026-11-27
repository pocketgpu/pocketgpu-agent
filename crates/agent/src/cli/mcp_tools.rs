//! `pgpu mcp`'s tools that need more than View (F32, US4): `offer_file` and `stop_job`
//! (Control) and `run_action` (Shell). Each goes to the agent with the assistant's id from
//! `--assistant`; the agent checks the level on every call and audits a refusal. Nothing is
//! checked here, so a refusal always comes from the agent.

use anyhow::{Result, bail};
use pocketgpu_proto::assistants::ToolCall;
use pocketgpu_proto::local::Response;
use serde_json::{Value, json};

/// The tools' names, as `tools/call` gets them.
pub(super) const NAMES: [&str; 3] = ["offer_file", "stop_job", "run_action"];

/// The call the arguments ask for. A relative path is read from the server's folder, the one
/// the assistant started it in.
pub(super) fn parse(name: &str, args: &Value) -> Result<ToolCall> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim);
    Ok(match name {
        "offer_file" => {
            let Some(path) = text("path").filter(|p| !p.is_empty()) else {
                bail!("give the path of the file to offer");
            };
            ToolCall::OfferFile {
                path: super::send::path_to_send(std::path::Path::new(path))?,
            }
        }
        "stop_job" => {
            let Some(number) = args
                .get("number")
                .or_else(|| args.get("job"))
                .and_then(Value::as_u64)
            else {
                bail!("give the job number, from list_jobs");
            };
            ToolCall::StopJob { number }
        }
        "run_action" => {
            let Some(name) = text("name").filter(|n| !n.is_empty()) else {
                bail!("give the saved action's name or number");
            };
            ToolCall::RunAction { name: name.into() }
        }
        other => bail!("no tool named {other}"),
    })
}

/// The agent's reply in words for the assistant.
pub(super) fn reply(call: &ToolCall, r: Response) -> Result<String> {
    match (call, r) {
        (_, Response::Error(e)) => bail!(e),
        (ToolCall::OfferFile { .. }, Response::Offered { name, to, .. }) => Ok(if to.is_empty() {
            format!(
                "{name} is offered for 7 days, but no phone may save it yet: the user can let \
                 one with `pgpu devices set <phone> control`."
            )
        } else {
            format!(
                "{name} is offered to {} for 7 days; they can save it from Files.",
                to.join(", ")
            )
        }),
        (ToolCall::StopJob { number }, Response::Ok) => Ok(format!(
            "Asked job {number} to stop: it gets SIGTERM, then SIGKILL after 10 s. Check it \
             with list_jobs."
        )),
        (
            ToolCall::RunAction { name },
            Response::ActionRan {
                exit_code,
                output,
                duration_ms,
                timed_out,
            },
        ) => {
            let how = match (timed_out, exit_code) {
                (true, _) => "timed out".to_string(),
                (false, Some(code)) => format!("exited with {code}"),
                (false, None) => "was killed by a signal".to_string(),
            };
            let secs = duration_ms as f64 / 1000.0;
            Ok(format!("{name} {how} after {secs:.1} s. Output:\n{output}"))
        }
        (_, other) => bail!("unexpected reply from the agent: {other:?}"),
    }
}

pub(super) fn tools() -> [Value; 3] {
    [
        json!({
            "name": "offer_file",
            "description": "Offer one file on this machine to the user's phones for 7 days, as `pgpu send` does: they get a push naming this machine (never the file) and save it from Files. Keys and secret files are refused. Needs the Control level for this assistant.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file, absolute or relative to the folder you started in." },
                },
                "required": ["path"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true },
        }),
        json!({
            "name": "stop_job",
            "description": "Stop a job you started with `pgpu run` in this folder during this session (SIGTERM, then SIGKILL after 10 s). Needs the Control level for this assistant.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "number": { "type": "integer", "description": "The job number from list_jobs." },
                },
                "required": ["number"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true },
        }),
        json!({
            "name": "run_action",
            "description": "Run one of the user's saved actions (`pgpu actions`) by name or number and get its exit code and output. Needs the Shell level for this assistant.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The action's name or number." },
                },
                "required": ["name"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "openWorldHint": true },
        }),
    ]
}
