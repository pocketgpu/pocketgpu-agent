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
                            None => prompt(ask, shell_enabled(paths).await).await?,
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
    // The QR code opens the app, not a web page: someone without it needs to know where to
    // get it before the code expires.
    println!("  No app yet? It's in beta: pocketgpu.app/#beta");
    if !std::io::stdout().is_terminal() {
        println!(
            "  Expires in {}:{:02}",
            info.expires_in_s / 60,
            info.expires_in_s % 60
        );
    }
    Ok(())
}

fn platform(ask: &PairAsk) -> &'static str {
    match ask.platform {
        Platform::Ios => "iOS",
        Platform::Android => "Android",
        Platform::Desktop => "desktop",
        Platform::Unknown => "unknown device",
    }
}

fn describe(ask: &PairAsk) -> String {
    format!(
        "\"{}\" ({}, key {})",
        ask.device_name,
        platform(ask),
        ask.device_fingerprint
    )
}

/// Whether terminal access is on, for the Shell line; None when the agent doesn't say.
async fn shell_enabled(paths: &Paths) -> Option<bool> {
    match client::call(paths, Request::GetShell).await {
        Ok(Response::Shell { enabled, .. }) => Some(enabled),
        _ => None,
    }
}

/// The question, with what each level allows. The key comes first, on its own line: the name
/// is whatever the phone chose to call itself.
fn menu(ask: &PairAsk, shell_on: Option<bool>, user: &str) -> String {
    let shell_off = if shell_on == Some(false) {
        "\n                (the terminal stays off here until you run: pgpu shell on)"
    } else {
        ""
    };
    format!(
        "  Phone key  {key}  (check it matches the phone's screen)\n\
         \x20 \"{name}\" ({platform}) wants to connect. What can it do on this machine?\n\
         \x20   v  View     see stats, jobs and alerts\n\
         \x20   c  Control  View, plus run quick actions, stop jobs and kill processes\n\
         \x20   s  Shell    Control, plus a terminal: it can run any command as {user}{shell_off}\n\
         \x20   d  Deny\n",
        key = ask.device_fingerprint,
        name = ask.device_name,
        platform = platform(ask),
    )
}

/// Asks on the terminal; Enter picks Control, the usual choice. End of input counts as a
/// denial.
async fn prompt(ask: PairAsk, shell_on: Option<bool>) -> Result<Option<Permission>> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "this user".into());
    let menu = menu(&ask, shell_on, &user);
    tokio::task::spawn_blocking(move || {
        let stdin = std::io::stdin();
        eprint!("{menu}");
        loop {
            eprint!("  Choose v, c, s or d [c]: ");
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
        "" | "c" | "control" => Some(Some(Permission::Control)),
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
        // Enter alone picks Control.
        assert_eq!(parse_answer("\n"), Some(Some(Permission::Control)));
    }

    #[test]
    fn the_menu_says_what_each_level_allows() {
        let ask = PairAsk {
            device_name: "Pixel 9".into(),
            platform: Platform::Android,
            device_fingerprint: "a91c 40de 77b2 19aa".into(),
        };
        let menu = menu(&ask, Some(false), "kami");
        // The key comes before anything the phone named itself.
        assert!(menu.starts_with("  Phone key  a91c 40de 77b2 19aa"));
        assert!(menu.contains("\"Pixel 9\" (Android) wants to connect"));
        assert!(menu.contains("any command as kami"));
        assert!(menu.contains("pgpu shell on"));
        assert!(!super::menu(&ask, Some(true), "kami").contains("pgpu shell on"));
    }
}
