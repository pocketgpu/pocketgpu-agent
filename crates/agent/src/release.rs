//! Checking an agent release before `pgpu update` installs it: the minisign signature on
//! `SHA256SUMS`, the archive's checksum, and the binary inside.
//!
//! Each release (`.github/releases-repo/release.yml`) publishes
//! `pocketgpu-<arch>-linux.tar.gz`, `pocketgpu-x86_64-windows.tar.gz` (Windows 10 and 11 have
//! `tar`), `pocketgpu-aarch64-macos.tar.gz` (Apple silicon), `SHA256SUMS` and `SHA256SUMS.minisig`, the same files the installers
//! (`web/static/install`, `web/static/install.ps1`) check.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

/// Public key for release signatures (minisign key EDBB333B4C2A9C4A, base64); the same as
/// `MINISIGN_PUBKEY` in `web/static/install`. The secret key is the repository secret
/// `MINISIGN_SECRET_KEY` and a backup kept offline.
pub const RELEASE_PUBKEY: &str = "RWQFc2OekDAK+RAphy78vna45XDYb5pbU3qu7B4d8ERcuzY82sYtkwV4";

/// Largest binary accepted from an archive.
const MAX_BINARY: u64 = 256 * 1024 * 1024;

/// The binary's name inside an archive.
const BINARY: &str = if cfg!(windows) {
    "pocketgpu.exe"
} else {
    "pocketgpu"
};

/// The release archive for this machine's system and CPU, or `None` where there are no releases.
pub fn archive_name() -> Option<String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", arch @ ("x86_64" | "aarch64")) => Some(format!("pocketgpu-{arch}-linux.tar.gz")),
        ("windows", "x86_64") => Some("pocketgpu-x86_64-windows.tar.gz".into()),
        ("macos", "aarch64") => Some("pocketgpu-aarch64-macos.tar.gz".into()),
        _ => None,
    }
}

/// Checks `sums` (the bytes of `SHA256SUMS`) against `SHA256SUMS.minisig`.
pub fn verify_signature(pubkey: &str, sums: &[u8], minisig: &str) -> Result<()> {
    if pubkey.is_empty() {
        bail!(
            "this build has no release signing key, so it can't check an update. \
             Reinstall with: curl -fsSL https://pocketgpu.app/install | sh"
        );
    }
    let key = minisign_verify::PublicKey::from_base64(pubkey)
        .map_err(|e| anyhow!("the built-in release key is invalid: {e}"))?;
    let signature = minisign_verify::Signature::decode(minisig)
        .map_err(|_| anyhow!("SHA256SUMS.minisig is not a minisign signature"))?;
    key.verify(sums, &signature, false).map_err(|_| {
        anyhow!(
            "the release signature doesn't match the key this pgpu trusts: the download is \
             damaged or was changed, or this pgpu is older than the current signing key. \
             Reinstall from https://pocketgpu.app"
        )
    })
}

/// The checksum `SHA256SUMS` lists for `file` (`sha256sum` format: hash, spaces, name).
pub fn listed_checksum<'a>(sums: &'a str, file: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim_start_matches([' ', '*']) == file).then_some(hash)
    })
}

/// Checks `data` against the checksum `sums` lists for `file`.
pub fn verify_checksum(sums: &str, file: &str, data: &[u8]) -> Result<()> {
    let want = listed_checksum(sums, file)
        .with_context(|| format!("SHA256SUMS has no entry for {file}"))?;
    let got = format!("{:x}", Sha256::digest(data));
    if !want.eq_ignore_ascii_case(&got) {
        bail!("checksum mismatch for {file}; the download is damaged or was changed");
    }
    Ok(())
}

/// The `pocketgpu` binary from a release archive.
pub fn extract_binary(archive: &[u8]) -> Result<Vec<u8>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar
        .entries()
        .context("the download is not a tar.gz archive")?
    {
        let entry = entry.context("the download is not a tar.gz archive")?;
        let is_binary = entry.header().entry_type().is_file()
            && entry.path().is_ok_and(|p| p.as_ref() == Path::new(BINARY));
        if !is_binary {
            continue;
        }
        if entry.header().size()? > MAX_BINARY {
            bail!("the pocketgpu binary in the download is too large");
        }
        let mut out = Vec::new();
        entry
            .take(MAX_BINARY)
            .read_to_end(&mut out)
            .context("could not unpack the download")?;
        return Ok(out);
    }
    bail!("the download has no pocketgpu binary")
}

/// Parses `v1.2.3` or `1.2.3`; a pre-release or build suffix is ignored.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// Whether release `tag` is newer than the running `current` version.
pub fn is_newer(tag: &str, current: &str) -> bool {
    match (parse_version(tag), parse_version(current)) {
        (Some(a), Some(b)) if a == b => is_prerelease(current) && !is_prerelease(tag),
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// `1.2.0-rc.1` comes before `1.2.0`.
pub fn is_prerelease(version: &str) -> bool {
    version.trim().trim_start_matches('v').contains('-')
}

/// The same version, `v` or not: `v0.1.0-rc.1` is `0.1.0-rc.1`.
pub fn same_version(a: &str, b: &str) -> bool {
    a.trim().trim_start_matches('v') == b.trim().trim_start_matches('v')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> (String, minisign::SecretKey) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        (kp.pk.to_base64(), kp.sk)
    }

    fn sign(sk: &minisign::SecretKey, data: &[u8]) -> String {
        minisign::sign(None, sk, data, None, None)
            .unwrap()
            .into_string()
    }

    fn archive(name: &str, body: &[u8]) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        builder.append_data(&mut header, name, body).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn accepts_only_the_signed_sums() {
        let (pk, sk) = keypair();
        let sums = b"abc  pocketgpu-x86_64-linux.tar.gz\n";
        let sig = sign(&sk, sums);
        verify_signature(&pk, sums, &sig).unwrap();

        let changed = b"abd  pocketgpu-x86_64-linux.tar.gz\n";
        let err = verify_signature(&pk, changed, &sig).unwrap_err();
        assert!(err.to_string().contains("doesn't match"), "{err}");

        let (other, _) = keypair();
        assert!(verify_signature(&other, sums, &sig).is_err());
        assert!(verify_signature(&pk, sums, "not a signature").is_err());
    }

    #[test]
    fn refuses_without_a_release_key() {
        let err = verify_signature("", b"", "").unwrap_err();
        assert!(err.to_string().contains("no release signing key"), "{err}");
    }

    #[test]
    fn checks_the_listed_checksum() {
        let data = b"release";
        let hash = format!("{:x}", Sha256::digest(data));
        let sums = format!("{hash}  pocketgpu-x86_64-linux.tar.gz\n0000 *other.tar.gz\n");
        verify_checksum(&sums, "pocketgpu-x86_64-linux.tar.gz", data).unwrap();
        assert_eq!(listed_checksum(&sums, "other.tar.gz"), Some("0000"));

        let err = verify_checksum(&sums, "pocketgpu-x86_64-linux.tar.gz", b"changed").unwrap_err();
        assert!(err.to_string().contains("mismatch"), "{err}");
        let err = verify_checksum(&sums, "pocketgpu-aarch64-linux.tar.gz", data).unwrap_err();
        assert!(err.to_string().contains("no entry"), "{err}");
    }

    #[test]
    fn extracts_the_binary() {
        let body = b"\x7fELF fake agent";
        assert_eq!(extract_binary(&archive(BINARY, body)).unwrap(), body);
        let err = extract_binary(&archive("README", body)).unwrap_err();
        assert!(err.to_string().contains("no pocketgpu binary"), "{err}");
        assert!(extract_binary(b"not gzip").is_err());
    }

    #[test]
    fn compares_versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.10.0-beta.1"), Some((1, 10, 0)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(!is_newer("v1.2.3", "1.2.3"));
        assert!(!is_newer("v1.2.2", "1.2.3"));
        assert!(!is_newer("nightly", "1.2.3"));
        assert!(is_newer("v0.1.0", "0.1.0-rc.1"));
        assert!(!is_newer("v0.1.0-rc.2", "0.1.0"));
        assert!(same_version("v0.1.0-rc.1", "0.1.0-rc.1"));
        assert!(!same_version("v0.1.0-rc.2", "0.1.0-rc.1"));
    }

    #[test]
    fn the_release_key_is_valid() {
        assert!(minisign_verify::PublicKey::from_base64(RELEASE_PUBKEY).is_ok());
    }
}
