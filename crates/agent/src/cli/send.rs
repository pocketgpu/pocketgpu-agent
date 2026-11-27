//! `pgpu send <file> [--to <device>]`: offers a file to your phones for 7 days (F40). They
//! get a push that names this machine, never the file, and can save it from Files.

use std::path::Path;

use anyhow::{Result, bail};
use pocketgpu_proto::local::{Request, Response};

use super::files::{size, text};
use crate::ipc::client;
use crate::store::db::now_ms;
use crate::store::paths::Paths;

pub async fn run(paths: &Paths, json: bool, file: &Path, to: Option<String>) -> Result<()> {
    let path = path_to_send(file)?;
    let offered = match client::call(paths, Request::Offer { path, to }).await? {
        Response::Offered {
            name,
            size,
            to,
            expires_ms,
            pushed,
        } => Offered {
            name,
            size,
            to,
            expires_ms,
            pushed,
        },
        Response::Error(e) => bail!(e),
        other => bail!("unexpected reply from the agent: {other:?}"),
    };
    if json {
        println!("{}", serde_json::to_string(&offered)?);
    } else {
        print!("{}", render(&offered, now_ms()));
    }
    Ok(())
}

/// The file as the agent gets it: absolute, with links kept. The agent checks both this path
/// and where it leads for keys and secrets; resolving links here would hide a secret behind
/// one (`~/.kube/config` is often a link to a cluster file elsewhere).
pub(super) fn path_to_send(file: &Path) -> Result<String> {
    let full = std::path::absolute(file)?;
    if !full.exists() {
        bail!("{} doesn't exist or can't be opened", file.display());
    }
    if !full.is_file() {
        bail!("{} is not a file; send one file at a time", file.display());
    }
    text(&full)
}

/// What the agent offered, and to whom.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Offered {
    pub name: String,
    pub size: u64,
    /// Devices that may save it.
    pub to: Vec<String>,
    pub expires_ms: i64,
    /// Those of them that get a push.
    pub pushed: Vec<String>,
}

pub fn render(o: &Offered, now_ms: i64) -> String {
    let Offered {
        name,
        size: bytes,
        to,
        expires_ms,
        pushed,
    } = o;
    let days = ((expires_ms - now_ms).max(0) + 43_200_000) / 86_400_000;
    let period = match days {
        0 => "for less than a day".to_string(),
        1 => "for 1 day".to_string(),
        n => format!("for {n} days"),
    };
    if to.is_empty() {
        return format!(
            "Offered {name} ({}) {period}, but no device can save it yet: pair a phone and \
             let it run actions first (pgpu pair).\n",
            size(*bytes)
        );
    }
    let then = if pushed.len() == to.len() {
        "They got a push and can save it from Files.".to_string()
    } else if pushed.is_empty() {
        "They can save it from Files.".to_string()
    } else {
        format!(
            "{} got a push; all of them can save it from Files.",
            and_list(pushed)
        )
    };
    format!(
        "Offered {name} ({}) to {} {period}.\n{then}\n",
        size(*bytes),
        and_list(to)
    )
}

/// `Pixel 8`, `Pixel 8 and iPad`, `Pixel 8, iPad and MacBook`.
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A secret behind a link: the path sent keeps the link, so the agent sees `.kube`.
    #[cfg(unix)]
    #[test]
    fn a_secret_reached_through_a_link_is_sent_as_typed() {
        use crate::files::secrets::any_secret;
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("clusters")).unwrap();
        std::fs::create_dir_all(home.join(".kube")).unwrap();
        let target = home.join("clusters/prod.yaml");
        std::fs::write(&target, b"token: x").unwrap();
        let link = home.join(".kube/config");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let homes = vec![home.clone()];

        let sent = path_to_send(&link).unwrap();
        assert_eq!(Path::new(&sent), link);
        assert!(any_secret(&[Path::new(&sent)], &homes));
        // Where it leads looks harmless: only the path as typed gives it away.
        assert!(!any_secret(&[target.as_path()], &homes));
        // `..` in the typed path doesn't hide it either.
        let roundabout = home.join("clusters/../.kube/config");
        let sent = path_to_send(&roundabout).unwrap();
        assert!(any_secret(&[Path::new(&sent)], &homes), "{sent}");

        assert!(path_to_send(&home.join("missing")).is_err());
        let err = path_to_send(&home).unwrap_err().to_string();
        assert!(err.contains("is not a file"), "{err}");
    }

    fn offered(to: &[&str], pushed: &[&str]) -> Offered {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect();
        Offered {
            name: "adapter.safetensors".into(),
            size: 320 << 20,
            to: names(to),
            expires_ms: 7 * 86_400_000,
            pushed: names(pushed),
        }
    }

    #[test]
    fn names_the_file_its_size_and_who_got_it() {
        let all = &["Pixel 8", "iPad"];
        let out = render(&offered(all, all), 0);
        assert_eq!(
            out,
            "Offered adapter.safetensors (320 MB) to Pixel 8 and iPad for 7 days.\n\
             They got a push and can save it from Files.\n"
        );
        let nobody = render(&offered(&[], &[]), 0);
        assert!(nobody.contains("pgpu pair"), "{nobody}");
        assert_eq!(
            and_list(&["A".into(), "B".into(), "C".into()]),
            "A, B and C"
        );
    }

    #[test]
    fn says_got_a_push_only_for_devices_that_get_one() {
        let none = render(&offered(&["MacBook"], &[]), 0);
        assert!(none.ends_with("\nThey can save it from Files.\n"), "{none}");
        let some = render(&offered(&["Pixel 8", "MacBook"], &["Pixel 8"]), 0);
        assert!(
            some.ends_with("\nPixel 8 got a push; all of them can save it from Files.\n"),
            "{some}"
        );
    }
}
