//! Files `pgpu send` never offers (F32 safety): keys, cloud credentials and `.env` files. An
//! AI assistant on this computer can run `pgpu send` too, so the agent refuses these itself,
//! whatever asked for them.

use std::path::{Component, Path};

/// Folders under the home folder that hold keys, cloud logins and tokens.
const SECRET_DIRS: [&str; 7] = [
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".config/gcloud",
    ".config/gh",
];
/// Key and certificate file endings.
const SECRET_ENDINGS: [&str; 5] = [".pem", ".key", ".p12", ".pfx", ".keystore"];
/// Private SSH key names, with or without an ending.
const KEY_PREFIXES: [&str; 4] = ["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"];
/// Whole file names: logins, and the plain-text token files of git, PyPI and npm.
const SECRET_NAMES: [&str; 7] = [
    ".netrc",
    "_netrc",
    "credentials",
    ".env",
    ".git-credentials",
    ".pypirc",
    ".npmrc",
];

/// The message when a path is refused.
pub fn refusal(shown: &str) -> String {
    format!("PocketGPU doesn't send keys or secrets: {shown}")
}

/// Whether this path looks like a key or secret, by its name or by being inside one of the
/// home folders that hold them. `home` is the user's home folder; give both the path as
/// typed and the resolved one, as a link can hide either.
pub fn is_secret(path: &Path, home: Option<&Path>) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let name = name.to_ascii_lowercase();
        if SECRET_NAMES.contains(&name.as_str())
            || name.starts_with(".env.")
            || SECRET_ENDINGS.iter().any(|e| name.ends_with(e))
            || KEY_PREFIXES.iter().any(|p| name.starts_with(p))
        {
            return true;
        }
    }
    let Some(home) = home else {
        return false;
    };
    SECRET_DIRS.iter().any(|d| inside(path, &home.join(d)))
}

/// Whether `path` is `dir` or inside it, comparing whole parts and ignoring case, since
/// macOS and Windows folders usually do. `.` and `..` are worked out first, so
/// `~/out/../.kube/config` is inside `~/.kube` (the resolved path is checked too, for links).
fn inside(path: &Path, dir: &Path) -> bool {
    let (path, dir) = (parts(path), parts(dir));
    path.len() >= dir.len() && path[..dir.len()] == dir[..]
}

/// The path's parts in lower case, with `.` dropped and `..` taking off the part before it.
fn parts(p: &Path) -> Vec<String> {
    let lower = |c: Component| c.as_os_str().to_string_lossy().to_lowercase();
    let mut root = Vec::new();
    let mut names = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => root.push(lower(c)),
            Component::CurDir => {}
            Component::ParentDir => {
                names.pop();
            }
            Component::Normal(_) => names.push(lower(c)),
        }
    }
    root.extend(names);
    root
}

/// The user's home folder, as given and resolved through links.
pub fn homes() -> Vec<std::path::PathBuf> {
    let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) else {
        return Vec::new();
    };
    let mut out = vec![home.clone()];
    if let Ok(real) = home.canonicalize()
        && real != home
    {
        out.push(real);
    }
    out
}

/// Whether any of these paths is a secret under any of these homes.
pub fn any_secret(paths: &[&Path], homes: &[std::path::PathBuf]) -> bool {
    paths
        .iter()
        .any(|p| is_secret(p, None) || homes.iter().any(|h| is_secret(p, Some(h.as_path()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(p: &str) -> bool {
        is_secret(Path::new(p), Some(Path::new("/home/ana")))
    }

    #[test]
    fn keys_and_credentials_are_secret() {
        for p in [
            "/home/ana/.ssh/config",
            "/home/ana/.ssh/known_hosts",
            "/home/ana/.aws/config",
            "/home/ana/.gnupg/pubring.kbx",
            "/home/ana/.kube/config",
            "/home/ana/.docker/config.json",
            "/home/ana/.config/gcloud/application_default_credentials.json",
            "/home/ana/.netrc",
            "/home/ana/proj/.env",
            "/home/ana/proj/.env.local",
            "/home/ana/proj/.ENV.production",
            "/srv/tls/server.pem",
            "/srv/tls/server.key",
            "/srv/tls/cert.p12",
            "/tmp/id_rsa",
            "/tmp/id_rsa.pub",
            "/backup/id_ed25519",
            "/backup/id_ecdsa_sk",
            "/home/ana/.aws/credentials",
            "/somewhere/credentials",
            "/home/ana/.SSH/id_x",
            "/home/ana/.git-credentials",
            "/home/ana/.config/gh/hosts.yml",
            "/home/ana/.pypirc",
            "/home/ana/proj/.npmrc",
            "/home/ana/out/../.kube/config",
            "/home/ana/./.aws/config",
        ] {
            assert!(secret(p), "{p} should be refused");
        }
    }

    #[test]
    fn ordinary_files_are_not() {
        for p in [
            "/home/ana/out/model.safetensors",
            "/home/ana/proj/.envrc.example.txt",
            "/home/ana/proj/environment.yml",
            "/home/ana/notes/keyboard.txt",
            "/home/ana/.sshrc-notes.txt",
            "/home/ana/docs/credentials.md",
            "/home/ana/.config/other/settings.json",
            "/home/ana/runs/loss.png",
            "/home/ana/.config/ghostty/config",
            "/home/ana/.kube/../notes.txt",
            "/home/ana/.git-credentials-help.txt",
        ] {
            assert!(!secret(p), "{p} should be allowed");
        }
    }

    #[test]
    fn a_link_to_a_key_is_caught_through_its_target() {
        let homes = vec![std::path::PathBuf::from("/home/ana")];
        let typed = Path::new("/home/ana/out/notes.txt");
        let real = Path::new("/home/ana/.ssh/id_x");
        assert!(any_secret(&[typed, real], &homes));
        assert!(!any_secret(&[typed], &homes));
    }
}
