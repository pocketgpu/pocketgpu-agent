//! `pgpu clip [get | put [--once] | clear | status | on | off]`: the clipboard for other
//! devices (F41). `pgpu clip` only reads and only `pgpu clip put` writes, so a script never
//! writes by accident: `export HF_TOKEN=$(pgpu clip)`, `tail -20 train.log | pgpu clip put`.

use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use pocketgpu_proto::clip::{MAX_TEXT, Place};
use pocketgpu_proto::local::{ClipStatus, Request, Response};
use tokio::io::AsyncReadExt;

use super::Silent;
use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Action {
    /// Print the clipboard's text exactly (the default); exits 1 when it's empty.
    Get,
    /// Put text from stdin on the clipboard: tail -20 train.log | pgpu clip put
    Put {
        /// Clear PocketGPU's own clipboard once the text is read.
        #[arg(long)]
        once: bool,
    },
    /// Empty the clipboard.
    Clear,
    /// Show whether other devices may use the clipboard, and where text goes.
    Status,
    /// Let phones that can run actions put text here, and phones with Full control read it.
    On,
    /// Stop other devices from using the clipboard.
    Off,
}

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let action = action.unwrap_or(Action::Get);
    let req = match action {
        Action::Get => return get(paths, json).await,
        Action::Put { once } => Request::ClipPut {
            text: read_stdin().await?,
            once,
        },
        Action::Clear => Request::ClipClear,
        Action::Status => Request::GetClip,
        Action::On => Request::SetClip { on: true },
        Action::Off => Request::SetClip { on: false },
    };
    let status = match client::call(paths, req).await? {
        Response::Clip(s) => s,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    let text = match action {
        Action::Put { once } => render_put(&status, once),
        Action::Clear => "The clipboard is empty.\n".to_string(),
        _ => render(&status),
    };
    print!("{text}");
    Ok(())
}

async fn get(paths: &Paths, json: bool) -> Result<()> {
    let text = match client::call(paths, Request::ClipGet).await? {
        Response::ClipText(text) => text,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::json!({ "text": text }));
    }
    match text {
        Some(text) => {
            if !json {
                print!("{text}");
            }
            Ok(())
        }
        None => {
            if !json {
                eprintln!("The clipboard is empty.");
            }
            Err(Silent(1).into())
        }
    }
}

/// Text from stdin, at most [`MAX_TEXT`] bytes.
async fn read_stdin() -> Result<String> {
    if std::io::stdin().is_terminal() {
        eprintln!("Type the text, then press Ctrl+D (Ctrl+Z, Enter on Windows).");
    }
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take(MAX_TEXT as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .context("could not read stdin")?;
    check(bytes)
}

fn check(bytes: Vec<u8>) -> Result<String> {
    if bytes.len() > MAX_TEXT {
        bail!(
            "the clipboard takes at most {} KB of text; this is more",
            MAX_TEXT / 1024
        );
    }
    if bytes.is_empty() {
        bail!("nothing to put: the text is empty. To empty the clipboard: pgpu clip clear");
    }
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("the clipboard takes text only"))
}

pub fn render(s: &ClipStatus) -> String {
    if !s.enabled {
        return "The clipboard is off: other devices can't put text here or read it.\n\
                Turn it on with: pgpu clip on\n"
            .into();
    }
    let mut out = String::from(
        "The clipboard is on. Phones that can run actions can put text here; phones with Full \
         control can read it.\n",
    );
    match s.place {
        Place::System => out.push_str("Text goes to this computer's own clipboard.\n"),
        Place::Memory => {
            out.push_str(
                "No screen here, so PocketGPU keeps its own clipboard in memory: read it with \
                 pgpu clip\n",
            );
            match (s.has_text, s.clears_in_s) {
                (true, Some(left)) => {
                    out.push_str(&format!("It holds text; it clears in {}.\n", minutes(left)))
                }
                (true, None) => out.push_str("It holds text.\n"),
                (false, _) => out.push_str("It's empty.\n"),
            }
        }
    }
    out.push_str("Turn off with: pgpu clip off\n");
    out
}

pub fn render_put(s: &ClipStatus, once: bool) -> String {
    let mut out = match (s.place, s.clears_in_s) {
        (Place::System, _) => "On this computer's clipboard.".to_string(),
        (Place::Memory, Some(left)) => {
            let read = if once { ", or once read" } else { "" };
            format!("Clears after {}{read}.", minutes(left))
        }
        (Place::Memory, None) => "On PocketGPU's clipboard.".to_string(),
    };
    out.push_str(if s.enabled {
        " Turn off with: pgpu clip off\n"
    } else {
        "\nPhones can't read it while the clipboard is off. Turn it on with: pgpu clip on\n"
    });
    out
}

/// `10 minutes`, `1 minute`; rounded up, so a few seconds left is still a minute.
fn minutes(s: u32) -> String {
    match s.div_ceil(60) {
        1 => "1 minute".into(),
        m => format!("{m} minutes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(enabled: bool, place: Place, has_text: bool) -> ClipStatus {
        ClipStatus {
            enabled,
            place,
            has_text,
            clears_in_s: (place == Place::Memory && has_text).then_some(600),
        }
    }

    #[test]
    fn put_says_when_it_clears() {
        let s = status(true, Place::Memory, true);
        assert_eq!(
            render_put(&s, true),
            "Clears after 10 minutes, or once read. Turn off with: pgpu clip off\n"
        );
        assert!(render_put(&s, false).starts_with("Clears after 10 minutes. "));
        let off = render_put(&status(false, Place::System, false), false);
        assert!(off.contains("pgpu clip on"), "{off}");
    }

    #[test]
    fn status_says_where_text_goes_and_how_to_change_it() {
        let mem = render(&status(true, Place::Memory, true));
        assert!(
            mem.contains("in memory") && mem.contains("clears in 10 minutes"),
            "{mem}"
        );
        assert!(mem.ends_with("Turn off with: pgpu clip off\n"), "{mem}");
        let sys = render(&status(true, Place::System, false));
        assert!(sys.contains("this computer's own clipboard"), "{sys}");
        let off = render(&status(false, Place::Memory, false));
        assert!(off.ends_with("Turn it on with: pgpu clip on\n"), "{off}");
    }

    #[test]
    fn put_takes_text_up_to_the_limit() {
        assert_eq!(check(b"hf_abc\n".to_vec()).unwrap(), "hf_abc\n");
        assert!(check(vec![b'a'; MAX_TEXT]).is_ok());
        let too_long = check(vec![b'a'; MAX_TEXT + 1]).unwrap_err().to_string();
        assert!(too_long.contains("256 KB"), "{too_long}");
        assert!(check(Vec::new()).is_err());
        assert!(check(vec![0xff, 0xfe]).is_err());
        assert_eq!(minutes(1), "1 minute");
        assert_eq!(minutes(61), "2 minutes");
    }
}
