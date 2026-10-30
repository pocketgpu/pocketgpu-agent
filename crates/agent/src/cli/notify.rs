//! `pgpu notify`: alert channels besides push (F20). Alerts go straight from this machine
//! to ntfy, Telegram, Discord or a webhook, without the PocketGPU push server.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{Channel, Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Show the channels (the default).
    List,
    /// Add a channel.
    Add {
        #[command(subcommand)]
        channel: Kind,
    },
    /// Remove a channel by its number in `pgpu notify`.
    Rm { number: u32 },
    /// Send a test alert to every channel.
    Test,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Kind {
    /// An ntfy topic (on ntfy.sh), or the full URL of a topic on your own ntfy server.
    Ntfy { topic: String },
    /// A Telegram bot token from @BotFather and the chat to post in. Give `-` as the token
    /// to type it instead: other users on this machine can see command-line arguments.
    Telegram { bot_token: String, chat_id: String },
    /// A Discord webhook URL (channel settings → Integrations → Webhooks), or `-` to type it.
    Discord { url: String },
    /// Any URL: gets a JSON POST with title, body, kind and machine. `-` to type it.
    Webhook { url: String },
}

/// `value`, or a line read from the terminal (or a pipe) when it is `-`, so the secret
/// isn't in the process list.
fn secret(value: String, what: &str) -> Result<String> {
    if value != "-" {
        return Ok(value);
    }
    eprint!("{what}: ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim().to_string();
    if line.is_empty() {
        anyhow::bail!("no {what} given");
    }
    Ok(line)
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let req = match action.unwrap_or(Action::List) {
        Action::List => Request::ListChannels,
        Action::Add { channel } => Request::AddChannel(match channel {
            Kind::Ntfy { topic } => Channel::Ntfy { topic },
            Kind::Telegram { bot_token, chat_id } => Channel::Telegram {
                bot_token: secret(bot_token, "Telegram bot token")?,
                chat_id,
            },
            Kind::Discord { url } => Channel::Discord {
                url: secret(url, "Discord webhook URL")?,
            },
            Kind::Webhook { url } => Channel::Webhook {
                url: secret(url, "Webhook URL")?,
            },
        }),
        Action::Rm { number } => Request::RemoveChannel { number },
        Action::Test => Request::TestChannels,
    };
    match client::call(paths, req).await? {
        Response::Channels(list) => {
            if json {
                println!("{}", serde_json::json!({ "channels": list }));
            } else {
                print!("{}", render(&list));
            }
        }
        Response::ChannelTest(results) => {
            if json {
                let out: Vec<_> = results
                    .iter()
                    .map(
                        |(c, e)| serde_json::json!({ "channel": c, "ok": e.is_none(), "error": e }),
                    )
                    .collect();
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else if results.is_empty() {
                println!("No channels yet. Add one with: pgpu notify add ntfy <topic>");
            } else {
                for (c, e) in &results {
                    match e {
                        None => println!("✓ {c}"),
                        Some(e) => println!("✗ {c}: {e}"),
                    }
                }
            }
            if results.iter().any(|(_, e)| e.is_some()) {
                return Err(crate::cli::Silent(1).into());
            }
        }
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    }
    Ok(())
}

pub fn render(list: &[String]) -> String {
    if list.is_empty() {
        return "No alert channels. Alerts go to paired phones only.\n\
                Add one with: pgpu notify add ntfy <topic>   (or telegram, discord, webhook)\n"
            .into();
    }
    let mut out = String::new();
    for (i, c) in list.iter().enumerate() {
        out += &format!("{:>2}  {c}\n", i + 1);
    }
    out + "Test them with: pgpu notify test\n"
}
