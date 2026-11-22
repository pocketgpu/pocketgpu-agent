//! Ollama's own signature on what setup downloaded, checked before anything of it runs. A file
//! the agent downloads skips the checks the system makes on a browser download (Gatekeeper,
//! SmartScreen), so they are made here: the Windows installer must carry a valid Authenticode
//! signature from Ollama, and Ollama.app a valid Developer ID signature from Ollama's Apple
//! team. Linux builds carry no signature.

/// The signer's name (CN) on Ollama's Windows installer, from its Authenticode certificate.
#[cfg(any(windows, test))]
pub const WINDOWS_PUBLISHER: &str = "Ollama Inc.";
/// The Apple team that signs Ollama.app ("Developer ID Application: Infra Technologies, Inc").
#[cfg(any(target_os = "macos", test))]
pub const MAC_TEAM_ID: &str = "3MU9H2V9Y9";

/// Checks Ollama's Windows installer before it runs.
#[cfg(windows)]
pub fn windows_installer(file: &std::path::Path) -> Result<(), String> {
    // The path goes in through the environment, never into the script's text.
    let script = "$s = Get-AuthenticodeSignature -LiteralPath $env:PGPU_SIGNED_FILE; \
        $n = if ($s.SignerCertificate) { $s.SignerCertificate.GetNameInfo('SimpleName', $false) } else { '' }; \
        [Console]::Out.Write([string]$s.Status + \"`n\" + $n)";
    let out = crate::winsys::quiet(r"WindowsPowerShell\v1.0\powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("PGPU_SIGNED_FILE", file)
        .output()
        .map_err(|e| format!("couldn't check the installer's signature: {e}"))?;
    windows_verdict(&String::from_utf8_lossy(&out.stdout))
}

/// `Valid`, then the signer's name, as the script above prints them.
#[cfg(any(windows, test))]
fn windows_verdict(out: &str) -> Result<(), String> {
    let mut lines = out.lines().map(str::trim);
    let status = lines.next().unwrap_or_default();
    let signer = lines.next().unwrap_or_default();
    if status != "Valid" {
        return Err(format!(
            "the Ollama installer isn't validly signed ({}), so it wasn't run",
            if status.is_empty() {
                "no answer"
            } else {
                status
            }
        ));
    }
    if signer != WINDOWS_PUBLISHER {
        return Err(format!(
            "the Ollama installer is signed by {signer:?}, not {WINDOWS_PUBLISHER:?}, so it wasn't run"
        ));
    }
    Ok(())
}

/// Checks Ollama.app, whole and with the system's strictest rules, before it opens.
#[cfg(target_os = "macos")]
pub fn mac_app(app: &std::path::Path) -> Result<(), String> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "-R", &mac_requirement()])
        .arg(app)
        .output()
        .map_err(|e| format!("couldn't check Ollama's signature: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Ollama.app isn't signed by Ollama ({}), so it wasn't opened",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A Developer ID signature (Apple's chain) from Ollama's team.
#[cfg(any(target_os = "macos", test))]
fn mac_requirement() -> String {
    format!("=anchor apple generic and certificate leaf[subject.OU] = \"{MAC_TEAM_ID}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_installer_needs_a_valid_signature_from_ollama() {
        assert!(windows_verdict("Valid\r\nOllama Inc.").is_ok());
        let other = windows_verdict("Valid\nSomeone Else LLC").unwrap_err();
        assert!(other.contains("Someone Else LLC"), "{other}");
        let bad = windows_verdict("HashMismatch\nOllama Inc.").unwrap_err();
        assert!(bad.contains("HashMismatch"), "{bad}");
        assert!(windows_verdict("NotSigned\n").is_err());
        assert!(windows_verdict("").is_err());
    }

    #[test]
    fn the_mac_requirement_names_ollamas_team() {
        assert_eq!(
            mac_requirement(),
            "=anchor apple generic and certificate leaf[subject.OU] = \"3MU9H2V9Y9\""
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unsigned_app_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Ollama.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/Ollama"), b"#!/bin/sh\n").unwrap();
        assert!(mac_app(&app).is_err());
    }
}
