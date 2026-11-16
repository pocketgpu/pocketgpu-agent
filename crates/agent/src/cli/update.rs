//! `pgpu update [--check] [--version vX.Y.Z]`: install a signed new agent release (F01).
//!
//! Downloads the release for this CPU, checks the minisign signature on `SHA256SUMS` and the
//! archive's checksum, makes sure the new binary runs here, then swaps it in and restarts the
//! service. Inside a container the image is updated instead.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::release;

/// Public GitHub repository with the release files (the code repository is private), as in
/// `web/static/install`; `PGPU_REPO` overrides it.
const DEFAULT_REPO: &str = "pocketgpu/pocketgpu-releases";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!("pocketgpu/", env!("CARGO_PKG_VERSION"));

pub async fn run(json: bool, check: bool, version: Option<&str>) -> Result<()> {
    if crate::daemon::root::in_container() {
        bail!(
            "this agent runs in a container; pull the new image instead: docker pull ghcr.io/pocketgpu/pocketgpu"
        );
    }
    let repo = repo();
    let client = client()?;
    let tag = match version {
        Some(v) => format!("v{}", v.trim_start_matches('v')),
        None => latest_tag(&repo).await?,
    };
    if release::parse_version(&tag).is_none() {
        bail!("{tag} is not a release version");
    }
    // An explicit --version may go back to an older release; the latest only ever goes forward.
    let wanted = if version.is_some() {
        !release::same_version(&tag, CURRENT)
    } else {
        release::is_newer(&tag, CURRENT)
    };

    if check || !wanted {
        if json {
            let out = serde_json::json!({ "current": CURRENT, "latest": tag, "update": wanted });
            println!("{out}");
        } else if wanted {
            println!(
                "pgpu {CURRENT} is installed; {tag} is available. Install it with: pgpu update"
            );
        } else {
            println!("pgpu {CURRENT} is up to date");
        }
        return Ok(());
    }

    if !json {
        println!("Downloading {tag}");
    }
    let binary = fetch(&client, &repo, &tag).await?;

    let exe = std::env::current_exe().context("could not find the running pocketgpu binary")?;
    install(&exe, &binary, &tag)?;
    // On Windows a helper does the restart, so it completes even when this command runs in a
    // phone's terminal, which closes with the agent.
    #[cfg(windows)]
    let restarted =
        crate::cli::service::state() == "active" && crate::cli::service::restart_detached().is_ok();
    #[cfg(not(windows))]
    let restarted =
        crate::cli::service::state() == "active" && crate::cli::service::restart().is_ok();

    if json {
        let out =
            serde_json::json!({ "ok": true, "from": CURRENT, "to": tag, "restarted": restarted });
        println!("{out}");
    } else {
        println!("Updated pgpu {CURRENT} to {tag} in {}", exe.display());
        if restarted && cfg!(windows) {
            println!("Restarting the agent; a phone's terminal reconnects in a few seconds.");
        } else if restarted {
            println!("Restarted the agent service.");
        } else {
            println!("Restart the agent to use it: pgpu service install, or restart pgpu agent.");
        }
    }
    Ok(())
}

/// The repository with the release files.
pub fn repo() -> String {
    std::env::var("PGPU_REPO").unwrap_or_else(|_| DEFAULT_REPO.into())
}

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(300))
        .build()?)
}

/// A client for finding the latest release: it stops at the redirect that names the tag. It
/// follows only redirects to another `releases/latest`, where a renamed repository sends it.
fn lookup_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && attempt.url().path().ends_with("/releases/latest") {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()?)
}

/// Downloads `tag`'s release for this machine and returns its binary once the signature on
/// `SHA256SUMS` and the archive's checksum are good.
pub async fn fetch(client: &reqwest::Client, repo: &str, tag: &str) -> Result<Vec<u8>> {
    let file = release::archive_name()
        .with_context(|| format!("there is no release for {} CPUs", std::env::consts::ARCH))?;
    let base = format!("https://github.com/{repo}/releases/download/{tag}");
    let sums = download(client, &format!("{base}/SHA256SUMS")).await?;
    let signature = download(client, &format!("{base}/SHA256SUMS.minisig")).await?;
    release::verify_signature(
        release::RELEASE_PUBKEY,
        &sums,
        &String::from_utf8_lossy(&signature),
    )?;
    let archive = download(client, &format!("{base}/{file}")).await?;
    release::verify_checksum(&String::from_utf8_lossy(&sums), &file, &archive)?;
    release::extract_binary(&archive)
}

/// The latest release's tag. github.com answers `releases/latest` with a redirect to that
/// release, `…/releases/tag/v1.2.3`. GitHub's API says the same but allows only 60 lookups an
/// hour per public address, shared by everyone behind it (offices, phone carriers, cloud GPU
/// providers), so it is asked only when the redirect no longer looks like that.
pub async fn latest_tag(repo: &str) -> Result<String> {
    let client = lookup_client()?;
    let req = client.get(format!("https://github.com/{repo}/releases/latest"));
    let res = look_up(req, repo).await?;
    let location = res
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|l| l.to_str().ok());
    match location.map_or(Latest::Unknown, read_latest) {
        Latest::Release(tag) => Ok(tag),
        Latest::NoReleases => bail!("{repo} has no releases yet"),
        Latest::Unknown => {
            tracing::info!(
                status = %res.status(),
                ?location,
                "github.com's redirect to the latest release looks different; asking GitHub's API"
            );
            latest_tag_from_api(&client, repo).await
        }
    }
}

async fn latest_tag_from_api(client: &reqwest::Client, repo: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let req = client
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("accept", "application/vnd.github+json");
    let latest: Release = look_up(req, repo)
        .await?
        .json()
        .await
        .context("GitHub sent an unexpected reply")?;
    if release::parse_version(&latest.tag_name).is_none() {
        bail!(
            "the latest release, {}, is not a release version",
            latest.tag_name
        );
    }
    Ok(latest.tag_name)
}

/// Sends a lookup to GitHub. A 404 means the repository isn't there or has no release.
async fn look_up(req: reqwest::RequestBuilder, repo: &str) -> Result<reqwest::Response> {
    let res = req
        .send()
        .await
        .context("could not reach GitHub to look for an update")?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("{repo} has no releases yet");
    }
    res.error_for_status()
        .context("could not look up the latest release")
}

/// Where github.com's redirect for `releases/latest` points.
#[derive(Debug, PartialEq)]
enum Latest {
    /// The latest release's page, `…/releases/tag/v1.2.3`.
    Release(String),
    /// The list of releases, while there are none yet.
    NoReleases,
    /// Anywhere else, including a tag that isn't a version.
    Unknown,
}

/// Reads the `location` of github.com's redirect for `releases/latest`: the tag is the last
/// part of the path.
fn read_latest(location: &str) -> Latest {
    let path = location.split(['?', '#']).next().unwrap_or_default();
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if last == "releases" {
        Latest::NoReleases
    } else if release::parse_version(last).is_some() {
        Latest::Release(last.to_string())
    } else {
        Latest::Unknown
    }
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let res = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("could not download {url}"))?;
    Ok(res
        .bytes()
        .await
        .with_context(|| format!("could not download {url}"))?
        .to_vec())
}

/// Writes the new binary next to `exe`, checks that it runs and reports `tag`'s version, then
/// renames it over `exe`. A running agent keeps its old file until it restarts.
pub fn install(exe: &Path, binary: &[u8], tag: &str) -> Result<()> {
    let dir = exe
        .parent()
        .context("the running binary has no directory")?;
    let new = dir.join(if cfg!(windows) {
        ".pocketgpu.new.exe"
    } else {
        ".pocketgpu.new"
    });
    std::fs::write(&new, binary).with_context(|| {
        format!(
            "could not write to {}. If you installed pocketgpu there as root, reinstall with the install script",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    let version = tag.trim_start_matches('v');
    let out = Command::new(&new).arg("--version").output();
    let runs = matches!(&out, Ok(o) if o.status.success()
        && String::from_utf8_lossy(&o.stdout).split_whitespace().any(|w| w == version));
    if !runs {
        let _ = std::fs::remove_file(&new);
        bail!("the downloaded {tag} binary doesn't run on this machine; kept pgpu {CURRENT}");
    }
    replace(&new, exe)
}

#[cfg(unix)]
fn replace(new: &Path, exe: &Path) -> Result<()> {
    std::fs::rename(new, exe).with_context(|| format!("could not replace {}", exe.display()))
}

/// `pocketgpu.old.exe`, and the names used when that one was still running.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_old_binary(file_name: &str) -> bool {
    let name = file_name.to_ascii_lowercase();
    name.starts_with("pocketgpu.old") && name.ends_with(".exe")
}

/// Windows can't replace a running program, but it can rename one: the old binary moves
/// aside to `pocketgpu.old.exe`, and the next update removes it. One that something still
/// runs from (an agent that never restarted) can't be removed or replaced, so then the
/// binary moves aside under a name of its own.
#[cfg(windows)]
fn replace(new: &Path, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .context("the running binary has no directory")?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_name().to_str().is_some_and(is_old_binary) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut old = dir.join("pocketgpu.old.exe");
    if old.exists() {
        old = dir.join(format!("pocketgpu.old-{}.exe", crate::store::db::now_ms()));
    }
    std::fs::rename(exe, &old)
        .with_context(|| format!("could not move {} aside", exe.display()))?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e).with_context(|| format!("could not replace {}", exe.display()));
    }
    Ok(())
}

#[cfg(test)]
mod old_binary_tests {
    use super::is_old_binary;

    #[test]
    fn knows_the_old_binaries() {
        assert!(is_old_binary("pocketgpu.old.exe"));
        assert!(is_old_binary("PocketGPU.OLD.exe"));
        assert!(is_old_binary("pocketgpu.old-1762500000000.exe"));
        assert!(!is_old_binary("pocketgpu.exe"));
        assert!(!is_old_binary(".pocketgpu.new.exe"));
        assert!(!is_old_binary("pocketgpu.stale.exe"));
    }
}

#[cfg(test)]
mod latest_tests {
    use super::{Latest, read_latest};

    const RELEASES: &str = "https://github.com/pocketgpu/pocketgpu-releases/releases";

    #[test]
    fn reads_the_tag() {
        for (location, tag) in [
            (format!("{RELEASES}/tag/v1.2.31"), "v1.2.31"),
            (format!("{RELEASES}/tag/v1.2.31/"), "v1.2.31"),
            (format!("{RELEASES}/tag/v1.2.31?a=b#c"), "v1.2.31"),
            (format!("{RELEASES}/tag/v1.3.0-rc.1"), "v1.3.0-rc.1"),
            (
                "/pocketgpu/pocketgpu-releases/releases/tag/v2.0.0".into(),
                "v2.0.0",
            ),
        ] {
            let want = Latest::Release(tag.into());
            assert_eq!(read_latest(&location), want, "{location}");
        }
    }

    #[test]
    fn no_tag_means_no_releases() {
        assert_eq!(read_latest(RELEASES), Latest::NoReleases);
        assert_eq!(read_latest(&format!("{RELEASES}/")), Latest::NoReleases);
    }

    #[test]
    fn anything_else_is_unknown() {
        for location in [
            format!("{RELEASES}/tag/nightly"),
            format!("{RELEASES}/tag/v1.2"),
            format!("{RELEASES}/latest"),
            "https://github.com/login?return_to=%2Fpocketgpu%2Fpocketgpu-releases%2Freleases"
                .into(),
            "https://github.com/".into(),
            String::new(),
        ] {
            assert_eq!(read_latest(&location), Latest::Unknown, "{location}");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A stand-in binary: a shell script that prints `pgpu <version>`.
    fn script(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\necho pgpu {version}\n").into_bytes()
    }

    #[test]
    fn swaps_in_a_binary_that_runs() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("pocketgpu");
        std::fs::write(&exe, script("0.1.0")).unwrap();
        install(&exe, &script("0.2.0"), "v0.2.0").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), script("0.2.0"));
        assert!(!dir.path().join(".pocketgpu.new").exists());
    }

    #[test]
    fn keeps_the_old_binary_when_the_new_one_fails() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("pocketgpu");
        std::fs::write(&exe, script("0.1.0")).unwrap();
        for bad in [script("0.3.0"), b"\x7fELF not for this cpu".to_vec()] {
            let err = install(&exe, &bad, "v0.2.0").unwrap_err();
            assert!(err.to_string().contains("doesn't run"), "{err}");
            assert_eq!(std::fs::read(&exe).unwrap(), script("0.1.0"));
            assert!(!dir.path().join(".pocketgpu.new").exists());
        }
    }
}
