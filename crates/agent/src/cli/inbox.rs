//! `pgpu inbox`: files other devices sent to this machine (F40), newest first.

use anyhow::{Result, bail};
use pocketgpu_proto::local::{InboxFile, Request, Response};

use super::files::size;
use crate::ipc::client;
use crate::store::db::now_ms;
use crate::store::paths::Paths;

pub async fn run(paths: &Paths, json: bool) -> Result<()> {
    let inbox = match client::call(paths, Request::GetFiles).await? {
        Response::Files(s) => s.inbox,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    let files = match client::call(paths, Request::Inbox).await? {
        Response::InboxList(files) => files,
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::json!({ "inbox": inbox, "files": files }));
    } else {
        print!("{}", render(&inbox, &files, now_ms()));
    }
    Ok(())
}

pub fn render(inbox: &str, files: &[InboxFile], now_ms: i64) -> String {
    if files.is_empty() {
        return format!("The Inbox is empty. Files sent from your phone land in {inbox}.\n");
    }
    let mut out = format!("In {inbox}:\n");
    out += &format!(
        "{:<40} {:>9}  {:<16} {}\n",
        "NAME", "SIZE", "FROM", "ARRIVED"
    );
    for f in files {
        // Names come from other devices: cleaned before they reach a terminal.
        let name: String = crate::audit::clean(&f.name).chars().take(40).collect();
        let from = match f.from.as_str() {
            "" => "-".to_string(),
            from => crate::audit::clean(from).chars().take(16).collect(),
        };
        out += &format!(
            "{:<40} {:>9}  {:<16} {}\n",
            name,
            size(f.size),
            from,
            super::audit::ago(now_ms - f.at_ms)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_names_sizes_and_senders() {
        let files = vec![
            InboxFile {
                name: "IMG_2041.jpg".into(),
                size: 2_400_000,
                from: "Pixel 8".into(),
                at_ms: 1_000_000 - 5 * 60_000,
            },
            InboxFile {
                name: "data\u{1b}[2K.csv".into(),
                size: 12,
                from: String::new(),
                at_ms: 0,
            },
        ];
        let out = render("~/PocketGPU/Inbox", &files, 1_000_000);
        assert!(out.starts_with("In ~/PocketGPU/Inbox:\nNAME"), "{out}");
        assert!(out.contains("IMG_2041.jpg"), "{out}");
        assert!(out.contains("2.3 MB") && out.contains("Pixel 8") && out.contains("5m ago"));
        assert!(!out.contains('\u{1b}'), "{out}");
        let empty = render("~/PocketGPU/Inbox", &[], 0);
        assert!(empty.starts_with("The Inbox is empty."), "{empty}");
    }
}
