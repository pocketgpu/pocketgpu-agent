//! A stand-in phone for end-to-end tests (`.github/android/e2e.sh`): pairs with a machine by
//! its one-time code and prints what the machine answered.
//!
//! `cargo run -p pocketgpu-client --example fake_phone -- <code> [device name]`

use iroh::SecretKey;
use pocketgpu_client::{Client, PhoneInfo, Relays};
use pocketgpu_proto::Platform;
use pocketgpu_proto::msg::pair::Response;
use pocketgpu_proto::pairing::PairInput;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let code = args
        .next()
        .ok_or("usage: fake_phone <code> [device name]")?;
    let name = args.next().unwrap_or_else(|| "Test phone".into());
    let input = PairInput::parse(&code).map_err(|e| format!("{e:?}"))?;
    let client = Client::new(SecretKey::generate(), Relays::Default).await?;
    let info = PhoneInfo {
        device_name: name,
        platform: Platform::Android,
        push_ticket: None,
    };
    let mut pairing = client.pair(&input, info).await?;
    loop {
        match pairing.next().await? {
            Response::Pending { agent_name, .. } => println!("pending on {agent_name}"),
            Response::Approved { permission, .. } => {
                println!("approved as {}", permission.as_str());
                return Ok(());
            }
            Response::Denied => return Err("denied".into()),
            Response::Error(e) => return Err(e.message.into()),
        }
    }
}
