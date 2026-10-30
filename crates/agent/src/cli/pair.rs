//! `pgpu pair`: shows a QR code and a one-time code, then asks to approve the phone.
//! Running the command at the machine is what authorises the pairing.

use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::{Result, bail};
use pocketgpu_proto::local::{PairAsk, PairCodeInfo, Request, Response};
use pocketgpu_proto::{Permission, Platform, read_frame, write_frame};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::ipc::client;
use crate::store::paths::Paths;

pub async fn run(paths: &Paths, json: bool, preset: Option<Permission>) -> Result<()> {
    let stream = client::connect(paths).await?;
    let (reader, mut w) = tokio::io::split(stream);
    write_frame(&mut w, &Request::Pair).await?;
    let mut replies = forward(reader);

    let live = !json && std::io::stdout().is_terminal();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut deadline: Option<Instant> = None;
    loop {
        tokio::select! {
            reply = replies.recv() => {
                let Some(reply) = reply else {
                    bail!("the agent stopped while pairing");
                };
                match reply {
                    Response::PairCode(info) => {
                        show_code(&info, json)?;
                        deadline = Some(Instant::now() + Duration::from_secs(info.expires_in_s.into()));
                    }
                    Response::PairAsk(ask) => {
                        if live && deadline.take().is_some() {
                            println!();
                        }
                        let permission = match preset {
                            Some(p) => {
                                eprintln!("Pairing {} as {}.", describe(&ask), p.as_str());
                                Some(p)
                            }
                            None => prompt(ask).await?,
                        };
                        write_frame(&mut w, &Request::PairDecision { permission }).await?;
                    }
                    Response::Paired { device_name, permission } => {
                        if json {
                            let out = serde_json::json!({
                                "device": device_name,
                                "permission": permission.as_str(),
                            });
                            println!("{out}");
                        } else {
                            println!("✓ Paired \"{device_name}\" ({})", permission.as_str());
                        }
                        return Ok(());
                    }
                    Response::PairEnded(reason) | Response::Error(reason) => {
                        if live && deadline.is_some() {
                            println!();
                        }
                        bail!(reason)
                    }
                    other => bail!("unexpected reply from the agent: {other:?}"),
                }
            }
            _ = tick.tick(), if live && deadline.is_some() => {
                let left = deadline.map_or(0, |d| d.saturating_duration_since(Instant::now()).as_secs());
                print!("\r  Expires in {}:{:02} ", left / 60, left % 60);
                std::io::stdout().flush()?;
            }
        }
    }
}

fn show_code(info: &PairCodeInfo, json: bool) -> Result<()> {
    if json {
        // Stdout is kept for the final result.
        eprintln!(
            "{}",
            serde_json::json!({
                "code": info.code,
                "qr": info.qr,
                "expires_in_s": info.expires_in_s,
                "agent_fingerprint": info.agent_fingerprint,
            })
        );
        return Ok(());
    }
    // Light modules are drawn as blocks so the code reads correctly on dark terminals.
    let qr = QrCode::new(info.qr.as_bytes())?
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    println!("{qr}");
    println!("  Scan with the PocketGPU app");
    println!("  or enter code {}", info.code);
    println!("  Machine key {}", info.agent_fingerprint);
    if !std::io::stdout().is_terminal() {
        println!(
            "  Expires in {}:{:02}",
            info.expires_in_s / 60,
            info.expires_in_s % 60
        );
    }
    Ok(())
}

fn describe(ask: &PairAsk) -> String {
    let platform = match ask.platform {
        Platform::Ios => "iOS",
        Platform::Android => "Android",
        Platform::Desktop => "desktop",
        Platform::Unknown => "unknown device",
    };
    format!(
        "\"{}\" ({platform}, key {})",
        ask.device_name, ask.device_fingerprint
    )
}

/// Asks on the terminal. End of input counts as a denial.
async fn prompt(ask: PairAsk) -> Result<Option<Permission>> {
    // The key first, on its own line: the name is whatever the phone chose to call itself.
    let question = format!(
        "  Phone key  {}  (check it matches the phone's screen)\n  Allow {}?  [v]iew  [c]ontrol  [s]hell  [d]eny: ",
        ask.device_fingerprint,
        describe(&ask)
    );
    tokio::task::spawn_blocking(move || {
        let stdin = std::io::stdin();
        loop {
            eprint!("{question}");
            std::io::stderr().flush()?;
            let mut line = String::new();
            if stdin.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if let Some(answer) = parse_answer(&line) {
                return Ok(answer);
            }
        }
    })
    .await?
}

fn parse_answer(line: &str) -> Option<Option<Permission>> {
    match line.trim().to_ascii_lowercase().as_str() {
        "v" | "view" => Some(Some(Permission::View)),
        "c" | "control" => Some(Some(Permission::Control)),
        "s" | "shell" => Some(Some(Permission::Shell)),
        "d" | "deny" | "n" | "no" => Some(None),
        _ => None,
    }
}

/// Reads replies on a task of their own, so the countdown never interrupts a frame.
fn forward<R>(mut reader: R) -> mpsc::Receiver<Response>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::channel(4);
    tokio::spawn(async move {
        while let Ok(Some(reply)) = read_frame::<_, Response>(&mut reader).await {
            if tx.send(reply).await.is_err() {
                break;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_accept_letters_and_words() {
        assert_eq!(parse_answer("c\n"), Some(Some(Permission::Control)));
        assert_eq!(parse_answer(" View "), Some(Some(Permission::View)));
        assert_eq!(parse_answer("d"), Some(None));
        assert_eq!(parse_answer("maybe"), None);
    }
}
