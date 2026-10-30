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

pub async fn run(json: bool, check: bool, version: Option<&str>) -> Result<()> {
    if crate::daemon::root::in_container() {
        bail!(
            "this agent runs in a container; pull the new image instead: docker pull ghcr.io/pocketgpu/pocketgpu"
        );
    }
    let repo = std::env::var("PGPU_REPO").unwrap_or_else(|_| DEFAULT_REPO.into());
    let client = reqwest::Client::builder()
        .user_agent(concat!("pocketgpu/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()?;
    let tag = match version {
        Some(v) => format!("v{}", v.trim_start_matches('v')),
        None => latest_tag(&client, &repo).await?,
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

    let file = release::archive_name()
        .with_context(|| format!("there is no release for {} CPUs", std::env::consts::ARCH))?;
    let base = format!("https://github.com/{repo}/releases/download/{tag}");
    let sums = download(&client, &format!("{base}/SHA256SUMS")).await?;
    let signature = download(&client, &format!("{base}/SHA256SUMS.minisig")).await?;
    release::verify_signature(
        release::RELEASE_PUBKEY,
        &sums,
        &String::from_utf8_lossy(&signature),
    )?;
    if !json {
        println!("Downloading {file}");
    }
    let archive = download(&client, &format!("{base}/{file}")).await?;
    release::verify_checksum(&String::from_utf8_lossy(&sums), &file, &archive)?;
    let binary = release::extract_binary(&archive)?;

    let exe = std::env::current_exe().context("could not find the running pocketgpu binary")?;
    install(&exe, &binary, &tag)?;
    let restarted =
        crate::cli::service::state() == "active" && crate::cli::service::restart().is_ok();

    if json {
        let out =
            serde_json::json!({ "ok": true, "from": CURRENT, "to": tag, "restarted": restarted });
        println!("{out}");
    } else {
        println!("Updated pgpu {CURRENT} to {tag} in {}", exe.display());
        if restarted {
            println!("Restarted the agent service.");
        } else {
            println!("Restart the agent to use it: pgpu service install, or restart pgpu agent.");
        }
    }
    Ok(())
}

async fn latest_tag(client: &reqwest::Client, repo: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let res = client
        .get(&url)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .context("could not reach GitHub to look for an update")?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("{repo} has no releases yet");
    }
    let release: Release = res
        .error_for_status()
        .context("could not look up the latest release")?
        .json()
        .await
        .context("GitHub sent an unexpected reply")?;
    Ok(release.tag_name)
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

/// Windows can't replace a running program, but it can rename one: the old binary moves
/// aside to `pocketgpu.old.exe` and is removed by the next update.
#[cfg(windows)]
fn replace(new: &Path, exe: &Path) -> Result<()> {
    let old = exe.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)
        .with_context(|| format!("could not move {} aside", exe.display()))?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e).with_context(|| format!("could not replace {}", exe.display()));
    }
    Ok(())
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
