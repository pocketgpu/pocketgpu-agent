//! What may be shared or be the Inbox (F40 user story 4). Risky folders are refused in
//! plain words: the root, the home folder and the folders above it, anything holding or
//! inside PocketGPU's own config and keys, `~/.ssh` and other dot-folders, `AppData` and
//! `~/Library` (where Windows and macOS programs keep keys, passwords and cookies, and start
//! at login), and system folders. Folders in the home folder and on removable drives are
//! never system folders, wherever the system keeps those (`/var/home` on Fedora Atomic,
//! `/run/media` for USB disks). Paths must exist and are kept canonical, links resolved.

use std::path::{Path, PathBuf};

/// What the guard needs to know about this machine.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub home: Option<PathBuf>,
    /// The agent's config, data and runtime folders.
    pub agent_dirs: Vec<PathBuf>,
}

impl Context {
    pub fn of(agent: &crate::daemon::state::Agent) -> Self {
        let p = &agent.paths;
        Self {
            home: super::home(),
            agent_dirs: [&p.config_dir, &p.data_dir, &p.runtime_dir]
                .into_iter()
                .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()))
                .collect(),
        }
    }
}

/// A folder to share: its canonical path, or why it can't be shared.
pub fn shareable(path: &Path, cx: &Context) -> Result<PathBuf, String> {
    let canon = existing_folder(path)?;
    match refusal(&canon, cx, false) {
        Some(why) => Err(why),
        None => Ok(canon),
    }
}

/// A folder to be the Inbox: its canonical path, or why it can't be.
pub fn inbox(path: &Path, cx: &Context) -> Result<PathBuf, String> {
    let canon = existing_folder(path)?;
    match refusal(&canon, cx, true) {
        Some(why) => Err(why),
        None => Ok(canon),
    }
}

fn existing_folder(path: &Path) -> Result<PathBuf, String> {
    let canon = path
        .canonicalize()
        .map_err(|_| format!("{} doesn't exist or can't be opened", path.display()))?;
    if !canon.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    Ok(canon)
}

/// Why a canonical folder can't be shared (or be the Inbox), or `None` when it can.
pub fn refusal(path: &Path, cx: &Context, for_inbox: bool) -> Option<String> {
    let shown = path.display();
    if path.parent().is_none() || is_drive_root(path) {
        return Some(format!(
            "{shown} is the whole disk; share the folder that holds your files instead"
        ));
    }
    if let Some(home) = &cx.home
        && inside(home, path)
    {
        return Some(if same(home, path) {
            format!(
                "{shown} is your home folder, with your keys and settings; share the folder \
                 that holds your files instead"
            )
        } else {
            format!(
                "{shown} holds your home folder; share the folder that holds your files instead"
            )
        });
    }
    for dir in &cx.agent_dirs {
        if inside(dir, path) || inside(path, dir) {
            return Some(format!(
                "{shown} holds PocketGPU's own keys and settings, so it can't be shared"
            ));
        }
    }
    if let Some(dot) = path.components().find_map(|c| {
        let name = c.as_os_str().to_string_lossy();
        (name.starts_with('.') && name.len() > 1).then(|| name.into_owned())
    }) {
        return Some(format!(
            "{shown} is in {dot}, a hidden folder where programs keep keys and settings"
        ));
    }
    if let Some(place) = program_data(path, cx.home.as_deref()) {
        return Some(if for_inbox {
            format!(
                "{shown} is in {place}, where programs can start at login; pick a folder for \
                 your files instead"
            )
        } else {
            format!(
                "{shown} is in {place}, where programs keep keys, passwords and settings; \
                 share the folder that holds your files instead"
            )
        });
    }
    if let Some(sys) = system_folders().iter().find(|s| inside(path, Path::new(s)))
        && !exempt(path, cx.home.as_deref())
    {
        return Some(format!("{shown} is in {sys}, a system folder"));
    }
    None
}

fn system_folders() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            r"C:\Windows",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\ProgramData",
        ]
    }
    #[cfg(not(windows))]
    {
        &[
            "/etc",
            "/usr",
            "/bin",
            "/sbin",
            "/boot",
            "/dev",
            "/proc",
            "/sys",
            "/var",
            "/lib",
            "/lib32",
            "/lib64",
            "/run",
            "/System",
            "/Library",
            "/private/etc",
            "/private/var",
        ]
    }
}

/// Places for people's files inside the system folders: anything in the home folder
/// (`/var/home/ana` on Fedora Atomic desktops), other users' homes and `/mnt` there,
/// removable drives (`/run/media/ana/USB` on Fedora, Arch and openSUSE), and macOS's
/// per-user temporary files.
fn exempt(path: &Path, home: Option<&Path>) -> bool {
    if home.is_some_and(|h| inside(path, h)) {
        return true;
    }
    let mut places = vec!["/var/home", "/var/mnt", "/run/media"];
    if cfg!(target_os = "macos") {
        places.push("/private/var/folders");
    }
    places.into_iter().any(|p| inside(path, Path::new(p)))
}

/// Where programs keep their data: `AppData` on Windows (Startup is in it, so programs
/// start from there at login), `~/Library` on macOS (keychains, cookies, LaunchAgents).
/// Linux keeps these in dot-folders, refused already.
fn program_data(path: &Path, home: Option<&Path>) -> Option<String> {
    let home = home?;
    ["AppData", "Library"]
        .into_iter()
        .map(|d| home.join(d))
        .find(|d| inside(path, d))
        .map(|d| d.display().to_string())
}

fn is_drive_root(path: &Path) -> bool {
    // `C:\` or `\\?\C:\`: a prefix and the root, nothing else.
    path.components().all(|c| {
        matches!(
            c,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        )
    })
}

/// Whether `path` is `dir` or inside it. Without regard to case on Windows and macOS, whose
/// disks usually ignore it (refusing more is the safe side), and without `\\?\`.
pub fn inside(path: &Path, dir: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        let (p, d) = (folded(path), folded(dir));
        p.len() >= d.len() && p[..d.len()] == d[..]
    } else {
        path.starts_with(dir)
    }
}

fn same(a: &Path, b: &Path) -> bool {
    inside(a, b) && inside(b, a)
}

/// Lower-case components without a `\\?\` prefix, for comparing paths.
fn folded(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .map(|c| c.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(c))
        .filter(|c| !c.is_empty())
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn cx() -> Context {
        Context {
            home: Some("/home/ana".into()),
            agent_dirs: vec![
                "/home/ana/.config/pocketgpu".into(),
                "/srv/pgpu-data".into(),
            ],
        }
    }

    fn refused(path: &str) -> String {
        refusal(Path::new(path), &cx(), false).unwrap_or_default()
    }

    #[test]
    fn refuses_risky_folders_in_plain_words() {
        assert!(refused("/").contains("whole disk"));
        assert!(refused("/home/ana").contains("your home folder"));
        assert!(refused("/home").contains("holds your home folder"));
        assert!(refused("/home/ana/.ssh").contains("hidden folder"));
        assert!(refused("/home/ana/.cache/huggingface").contains(".cache"));
        assert!(refused("/srv").contains("PocketGPU's own"));
        assert!(refused("/srv/pgpu-data/jobs").contains("PocketGPU's own"));
        for sys in ["/etc", "/usr/share", "/var/log", "/proc/1", "/boot"] {
            assert!(refused(sys).contains("system folder"), "{sys}");
        }
    }

    #[test]
    fn allows_ordinary_folders() {
        for ok in [
            "/home/ana/ComfyUI/output",
            "/mnt/data/runs",
            "/tmp/out",
            "/home/anabel",
        ] {
            assert_eq!(refusal(Path::new(ok), &cx(), false), None, "{ok}");
        }
    }

    #[test]
    fn program_data_is_neither_shared_nor_the_inbox() {
        let why = refusal(Path::new("/home/ana/Library/LaunchAgents"), &cx(), true).unwrap();
        assert!(why.contains("start at login"), "{why}");
        let why = refusal(Path::new("/home/ana/AppData/Roaming"), &cx(), true).unwrap();
        assert!(why.contains("start at login"), "{why}");
        assert_eq!(refusal(Path::new("/home/ana/Inbox"), &cx(), true), None);
        // Keychains, browser profiles and cookies are no folders to share either.
        for risky in [
            "/home/ana/Library/Keychains",
            "/home/ana/Library/Application Support/Firefox/Profiles",
            "/home/ana/AppData/Roaming/Mozilla/Firefox/Profiles",
            "/home/ana/AppData",
        ] {
            let why = refusal(Path::new(risky), &cx(), false).unwrap_or_default();
            assert!(why.contains("keys, passwords"), "{risky}: {why}");
        }
    }

    #[test]
    fn homes_and_drives_in_system_folders_are_fine() {
        // Fedora Atomic desktops keep homes in /var/home; USB disks mount in /run/media.
        let atomic = Context {
            home: Some("/var/home/ana".into()),
            agent_dirs: vec!["/var/home/ana/.config/pocketgpu".into()],
        };
        let refused = |p: &str| refusal(Path::new(p), &atomic, false);
        assert_eq!(refused("/var/home/ana/ComfyUI/output"), None);
        assert_eq!(
            refusal(Path::new("/var/home/ana/Inbox"), &atomic, true),
            None
        );
        assert_eq!(refused("/var/mnt/data/models"), None);
        assert_eq!(refused("/run/media/ana/Data/models"), None);
        assert!(
            refused("/var/home/ana")
                .unwrap()
                .contains("your home folder")
        );
        assert!(
            refused("/var/home")
                .unwrap()
                .contains("holds your home folder")
        );
        assert!(refused("/var/home/ana/.ssh").unwrap().contains("hidden"));
        for sys in ["/var/log", "/var/lib/docker", "/run/user/1000", "/etc"] {
            let why = refused(sys).unwrap_or_default();
            assert!(why.contains("system folder"), "{sys}: {why}");
        }
    }

    #[test]
    fn checks_that_the_folder_exists() {
        let dir = tempfile::Builder::new()
            .prefix("pgpu-guard-")
            .tempdir()
            .unwrap();
        let cx = Context::default();
        let canon = shareable(dir.path(), &cx).unwrap();
        assert_eq!(canon, dir.path().canonicalize().unwrap());
        assert!(
            shareable(&dir.path().join("missing"), &cx)
                .unwrap_err()
                .contains("doesn't exist")
        );
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(shareable(&file, &cx).unwrap_err().contains("not a folder"));
        // A link to a risky folder is judged by where it leads.
        let link = dir.path().join("etc");
        std::os::unix::fs::symlink("/etc", &link).unwrap();
        assert!(shareable(&link, &cx).unwrap_err().contains("system folder"));
    }
}
