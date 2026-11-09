//! `pgpu notices [status | all | fewer]`: how many remote actions show a notice on this
//! computer's screen (F35). Only here: phones can't change it. There is no off.

use anyhow::{Result, bail};
use clap::Subcommand;
use pocketgpu_proto::local::{NoticeLevel, Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Action {
    /// Show the level (the default).
    Status,
    /// New devices, terminals, files, the clipboard, stopped processes, quick actions and
    /// containers.
    All,
    /// Only new devices, terminals, files sent here and clipboard reads.
    Fewer,
    /// Not possible: says why.
    Off,
}

pub const NO_OFF: &str = "Notices can't be turned off, so nobody can use this computer from \
                          elsewhere without you knowing. For fewer, use: pgpu notices fewer";

pub async fn run(paths: &Paths, json: bool, action: Option<Action>) -> Result<()> {
    let req = match action.unwrap_or(Action::Status) {
        Action::Status => Request::GetNotices,
        Action::All => Request::SetNotices(NoticeLevel::All),
        Action::Fewer => Request::SetNotices(NoticeLevel::Fewer),
        Action::Off => bail!(NO_OFF),
    };
    let level = match client::call(paths, req).await? {
        Response::Notices(level) => level,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::json!({ "notices": level.as_str() }));
    } else {
        print!("{}", render(level));
    }
    Ok(())
}

pub fn render(level: NoticeLevel) -> String {
    match level {
        NoticeLevel::All => "Notices: all remote actions (new devices, terminals, files, the \
                             clipboard, stopped processes, quick actions and containers).\n\
                             For fewer: pgpu notices fewer\n"
            .into(),
        NoticeLevel::Fewer => "Notices: only new devices, terminals, files sent here and \
                               clipboard reads.\n\
                               For all remote actions: pgpu notices all\n"
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_to_change_it_and_never_offers_off() {
        assert!(render(NoticeLevel::All).contains("pgpu notices fewer"));
        assert!(render(NoticeLevel::Fewer).contains("pgpu notices all"));
        assert!(NO_OFF.contains("can't be turned off"));
    }
}
