//! The PocketGPU app on Windows (F34) comes from the same release as the agent. When the agent
//! starts and finds that app installed for this user at an older version, it installs the app
//! of its own release, checked like an agent update (the signed `SHA256SUMS`, then the
//! installer's checksum), silently and for this user only. So updating the agent from a phone,
//! `pgpu update` or automatically updates the app on this PC too (board 29). Tried once per
//! agent version.

/// The Windows app's installer in every release.
pub const SETUP: &str = "PocketGPU-x86_64-setup.exe";

/// Whether the app at `installed` should move to the agent's `agent` version.
pub fn behind(installed: Option<&str>, agent: &str) -> bool {
    installed.is_some_and(|v| crate::release::is_newer(agent, v))
}

/// `DisplayVersion    REG_SZ    1.2.9` from `reg query` → `1.2.9`.
pub fn reg_value(output: &str, name: &str) -> Option<String> {
    output.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        (parts.next()? == name && parts.next()?.starts_with("REG_"))
            .then(|| parts.collect::<Vec<_>>().join(" "))
            .map(|v| v.trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    })
}

#[cfg(windows)]
pub use imp::keep_in_step;

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use anyhow::{Context, Result, bail};

    use super::{SETUP, behind, reg_value};
    use crate::cli::update as cli;
    use crate::release;
    use crate::winsys::quiet;

    const CURRENT: &str = env!("CARGO_PKG_VERSION");
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PocketGPU";
    /// Phones reconnect first; the app can wait.
    const AFTER_START: Duration = Duration::from_secs(20);

    fn reg(name: &str) -> Option<String> {
        let out = quiet("reg")
            .args(["query", KEY, "/v", name])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| reg_value(&String::from_utf8_lossy(&out.stdout), name))
            .flatten()
    }

    /// Runs once a while after the agent starts.
    pub async fn keep_in_step(data_dir: PathBuf) {
        tokio::time::sleep(AFTER_START).await;
        let installed = tokio::task::spawn_blocking(|| reg("DisplayVersion"))
            .await
            .ok()
            .flatten();
        if !behind(installed.as_deref(), CURRENT) {
            return;
        }
        // Once per agent version, so a failing installer isn't run at every start.
        let tried = data_dir.join(format!("desktop-app-{CURRENT}.tried"));
        if tried.exists() {
            return;
        }
        let _ = std::fs::write(&tried, b"");
        match update(&data_dir).await {
            Ok(()) => tracing::info!(
                from = installed.as_deref().unwrap_or(""),
                to = CURRENT,
                "updated the PocketGPU app on this PC"
            ),
            Err(e) => tracing::warn!("could not update the PocketGPU app on this PC: {e:#}"),
        }
    }

    async fn update(data_dir: &Path) -> Result<()> {
        let client = cli::client()?;
        let base = format!(
            "https://github.com/{}/releases/download/v{CURRENT}",
            cli::repo()
        );
        let get = |url: String| {
            let client = client.clone();
            async move {
                let res = client.get(&url).send().await?.error_for_status()?;
                anyhow::Ok(res.bytes().await?.to_vec())
            }
        };
        let sums = get(format!("{base}/SHA256SUMS")).await?;
        let sig = get(format!("{base}/SHA256SUMS.minisig")).await?;
        release::verify_signature(
            release::RELEASE_PUBKEY,
            &sums,
            &String::from_utf8_lossy(&sig),
        )?;
        let setup = get(format!("{base}/{SETUP}")).await?;
        release::verify_checksum(&String::from_utf8_lossy(&sums), SETUP, &setup)?;
        let path = data_dir.join(SETUP);
        std::fs::write(&path, &setup).context("could not save the app installer")?;
        let running = tokio::task::spawn_blocking(app_running)
            .await
            .unwrap_or(false);
        // `/S`: silent, for this user (the installer never asks for Administrator); `/R`:
        // open the app again afterwards when it was open.
        let mut args = vec!["/S"];
        if running {
            args.push("/R");
        }
        let status = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&path).args(&args).status()
        })
        .await??;
        let _ = std::fs::remove_file(data_dir.join(SETUP));
        if !status.success() {
            bail!("the app installer exited with {status}");
        }
        Ok(())
    }

    /// Whether the app runs now: an exe in its install folder that `tasklist` lists.
    fn app_running() -> bool {
        let Some(dir) = reg("InstallLocation") else {
            return false;
        };
        let Ok(entries) = std::fs::read_dir(dir.trim_matches('"')) else {
            return false;
        };
        entries.flatten().any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_app = name.to_ascii_lowercase().ends_with(".exe")
                && !name.to_ascii_lowercase().starts_with("uninstall");
            is_app
                && quiet("tasklist")
                    .args(["/FI", &format!("IMAGENAME eq {name}"), "/NH"])
                    .output()
                    .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&name))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_older_installed_app_is_updated() {
        assert!(behind(Some("1.2.9"), "1.2.10"));
        assert!(!behind(Some("1.2.10"), "1.2.10"));
        assert!(!behind(Some("1.3.0"), "1.2.10"));
        assert!(!behind(None, "1.2.10"));
    }

    #[test]
    fn reads_a_registry_value() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\...\\PocketGPU\r\n    DisplayVersion    REG_SZ    1.2.9\r\n\r\n";
        assert_eq!(reg_value(out, "DisplayVersion").as_deref(), Some("1.2.9"));
        let loc =
            "    InstallLocation    REG_SZ    \"C:\\Users\\gan\\AppData\\Local\\PocketGPU\"\r\n";
        assert_eq!(
            reg_value(loc, "InstallLocation").as_deref(),
            Some("C:\\Users\\gan\\AppData\\Local\\PocketGPU")
        );
        assert_eq!(reg_value(out, "Missing"), None);
    }
}
