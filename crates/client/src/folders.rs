//! What may be shared or be the Inbox on a machine (F40 user story 4), and where synced
//! files may land on a device (F43). Risky folders are refused in plain words: the root,
//! the home folder and the folders above it, anything holding or inside PocketGPU's own
//! config and keys, `~/.ssh` and other dot-folders, `AppData` and `~/Library` (where Windows
//! and macOS programs keep keys, passwords and cookies, and start at login), and system
//! folders. Folders in the home folder and on removable drives are never system folders,
//! wherever the system keeps those (`/var/home` on Fedora Atomic, `/run/media` for USB
//! disks). Paths are kept canonical, links resolved. The agent and the app share these.

use std::path::{Path, PathBuf};

/// What the guard needs to know about this computer.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub home: Option<PathBuf>,
    /// PocketGPU's own config, data and runtime folders (the agent's, or the app's).
    pub agent_dirs: Vec<PathBuf>,
}

/// What a folder is for: the words differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// A shared folder on the machine.
    Share,
    /// The machine's Inbox.
    Inbox,
    /// Where a synced folder's files land on this device.
    Keep,
}

/// The user's home folder, canonical.
pub fn home() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(home.canonicalize().unwrap_or(home))
}

/// A folder to share: its canonical path, or why it can't be shared.
pub fn shareable(path: &Path, cx: &Context) -> Result<PathBuf, String> {
    let canon = existing_folder(path)?;
    match refusal(&canon, cx, Use::Share) {
        Some(why) => Err(why),
        None => Ok(canon),
    }
}

/// A folder to be the Inbox: its canonical path, or why it can't be.
pub fn inbox(path: &Path, cx: &Context) -> Result<PathBuf, String> {
    let canon = existing_folder(path)?;
    match refusal(&canon, cx, Use::Inbox) {
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

/// Why a canonical folder can't be used this way, or `None` when it can. The words for
/// sharing and the Inbox stay as they are: the app matches them (`this-pc/sharing.ts`).
pub fn refusal(path: &Path, cx: &Context, used: Use) -> Option<String> {
    let shown = path.display();
    let instead = match used {
        Use::Keep => "pick a folder like Documents or Desktop instead",
        _ => "share the folder that holds your files instead",
    };
    if path.parent().is_none() || is_drive_root(path) {
        return Some(format!("{shown} is the whole disk; {instead}"));
    }
    if let Some(home) = &cx.home
        && inside(home, path)
    {
        return Some(if same(home, path) {
            format!("{shown} is your home folder, with your keys and settings; {instead}")
        } else {
            format!("{shown} holds your home folder; {instead}")
        });
    }
    for dir in &cx.agent_dirs {
        if inside(dir, path) || inside(path, dir) {
            let cant = match used {
                Use::Keep => "files can't be kept there",
                _ => "it can't be shared",
            };
            return Some(format!(
                "{shown} holds PocketGPU's own keys and settings, so {cant}"
            ));
        }
    }
    if let Some(dot) = path.components().find_map(|c| {
        let name = c.as_os_str().to_string_lossy();
        (name.starts_with('.') && name.len() > 1).then(|| name.into_owned())
    }) {
        return Some(format!(
            "{shown} is in {dot}, a hidden folder where programs keep keys and settings; \
             {instead}"
        ));
    }
    if let Some(place) = program_data(path, cx.home.as_deref())
        && !(used == Use::Keep && cloud_drive(path, cx.home.as_deref()))
    {
        return Some(if used == Use::Share {
            format!(
                "{shown} is in {place}, where programs keep keys, passwords and settings; \
                 {instead}"
            )
        } else {
            let instead = match used {
                Use::Keep => instead,
                _ => "pick a folder for your files instead",
            };
            format!("{shown} is in {place}, where programs can start at login; {instead}")
        });
    }
    if let Some(sys) = system_folders().iter().find(|s| inside(path, Path::new(s)))
        && !exempt(path, cx.home.as_deref())
    {
        return Some(format!("{shown} is in {sys}, a system folder; {instead}"));
    }
    None
}

/// A folder here for a synced folder's files (F43): its canonical path, or why files can't
/// be kept there. It may not exist yet: the part that does is resolved, the rest added.
/// On Windows it comes back as people write it (`C:\Users\ana\runs`), without the `\\?\`
/// that canonical paths start with: it is shown, opened in Explorer and compared with
/// folders saved before.
pub fn landing(path: &Path, cx: &Context) -> Result<PathBuf, String> {
    let cant = || format!("{} can't be opened", path.display());
    if !path.is_absolute() {
        return Err(cant());
    }
    let mut base = path.to_path_buf();
    let mut rest = Vec::new();
    let mut full = loop {
        match base.canonicalize() {
            Ok(c) => break c,
            Err(_) => {
                rest.push(base.file_name().ok_or_else(cant)?.to_owned());
                if !base.pop() {
                    return Err(cant());
                }
            }
        }
    };
    if !full.is_dir() {
        return Err(format!("{} is not a folder", full.display()));
    }
    full.extend(rest.iter().rev());
    match refusal(&full, cx, Use::Keep) {
        Some(why) => Err(why),
        None => Ok(dunce::simplified(&full).to_path_buf()),
    }
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

/// iCloud Drive (`~/Library/Mobile Documents`) and the folders Dropbox, Google Drive,
/// OneDrive and Box keep on a Mac (`~/Library/CloudStorage`): synced files may land there.
fn cloud_drive(path: &Path, home: Option<&Path>) -> bool {
    let plain = path
        .components()
        .all(|c| !matches!(c, std::path::Component::ParentDir));
    plain
        && home.is_some_and(|h| {
            ["Library/Mobile Documents", "Library/CloudStorage"]
                .iter()
                .any(|d| inside(path, &h.join(d)))
        })
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
        refusal(Path::new(path), &cx(), Use::Share).unwrap_or_default()
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
            assert_eq!(refusal(Path::new(ok), &cx(), Use::Share), None, "{ok}");
        }
    }

    #[test]
    fn program_data_is_neither_shared_nor_the_inbox() {
        let why = refusal(
            Path::new("/home/ana/Library/LaunchAgents"),
            &cx(),
            Use::Inbox,
        )
        .unwrap();
        assert!(why.contains("start at login"), "{why}");
        let why = refusal(Path::new("/home/ana/AppData/Roaming"), &cx(), Use::Inbox).unwrap();
        assert!(why.contains("start at login"), "{why}");
        assert_eq!(
            refusal(Path::new("/home/ana/Inbox"), &cx(), Use::Inbox),
            None
        );
        // Keychains, browser profiles and cookies are no folders to share either.
        for risky in [
            "/home/ana/Library/Keychains",
            "/home/ana/Library/Application Support/Firefox/Profiles",
            "/home/ana/AppData/Roaming/Mozilla/Firefox/Profiles",
            "/home/ana/AppData",
        ] {
            let why = refusal(Path::new(risky), &cx(), Use::Share).unwrap_or_default();
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
        let refused = |p: &str| refusal(Path::new(p), &atomic, Use::Share);
        assert_eq!(refused("/var/home/ana/ComfyUI/output"), None);
        assert_eq!(
            refusal(Path::new("/var/home/ana/Inbox"), &atomic, Use::Inbox),
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

    #[test]
    fn synced_files_land_only_in_ordinary_folders() {
        let dir = tempfile::Builder::new()
            .prefix("pgpu-landing-")
            .tempdir()
            .unwrap();
        let home = dir.path().canonicalize().unwrap();
        let cx = Context {
            home: Some(home.clone()),
            agent_dirs: vec![home.join("Library/Application Support/PocketGPU")],
        };
        // Not there yet: made later.
        let ok = home.join("Documents/PocketGPU/rig/runs");
        assert_eq!(landing(&ok, &cx), Ok(ok));
        for (risky, why) in [
            (home.clone(), "your home folder"),
            (home.join(".ssh"), "hidden folder"),
            (home.join("Library/LaunchAgents"), "start at login"),
            (home.join("AppData/Roaming/Microsoft"), "start at login"),
            (
                home.join("Library/Application Support/PocketGPU/x"),
                "PocketGPU's own",
            ),
            ("/".into(), "whole disk"),
            ("/etc/x".into(), "system folder"),
        ] {
            let got = landing(&risky, &cx).unwrap_err();
            assert!(got.contains(why), "{}: {got}", risky.display());
        }
        assert!(landing(Path::new("runs"), &cx).is_err());
        // Each refusal says what to do instead.
        let why = landing(&home.join(".ssh"), &cx).unwrap_err();
        assert!(why.contains("like Documents or Desktop"), "{why}");
    }

    #[test]
    fn synced_files_may_land_in_cloud_drives() {
        let home = Path::new("/home/ana");
        for ok in [
            "/home/ana/Library/Mobile Documents/com~apple~CloudDocs/runs",
            "/home/ana/Library/CloudStorage/Dropbox/runs",
            "/home/ana/Library/CloudStorage/GoogleDrive-ana@example.com/My Drive",
            "/home/ana/Library/CloudStorage/OneDrive-Personal",
        ] {
            assert_eq!(refusal(Path::new(ok), &cx(), Use::Keep), None, "{ok}");
            // Only for synced files: sharing it or the Inbox there stays refused.
            assert!(refusal(Path::new(ok), &cx(), Use::Share).is_some(), "{ok}");
            assert!(refusal(Path::new(ok), &cx(), Use::Inbox).is_some(), "{ok}");
        }
        for risky in [
            "/home/ana/Library",
            "/home/ana/Library/LaunchAgents",
            "/home/ana/Library/Application Support/x",
        ] {
            let why = refusal(Path::new(risky), &cx(), Use::Keep).unwrap_or_default();
            assert!(why.contains("start at login"), "{risky}: {why}");
        }
        let up = Path::new("/home/ana/Library/Mobile Documents/../LaunchAgents");
        assert!(!cloud_drive(up, Some(home)));
        assert!(refusal(up, &cx(), Use::Keep).is_some());
        assert!(cloud_drive(
            &home.join("Library/CloudStorage/Box-Box"),
            Some(home)
        ));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn landing_paths_read_as_people_write_them() {
        // Not tempfile's usual `.tmp…` name: `landing` refuses hidden folders.
        let dir = tempfile::Builder::new().prefix("runs-").tempdir().unwrap();
        let want = dir.path().join("runs");
        let got = landing(&want, &Context::default()).unwrap_or_else(|e| panic!("{e}"));
        // Canonical, but `C:\…`, not `\\?\C:\…`.
        assert!(
            !got.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            got.display()
        );
        assert_eq!(got, dunce::canonicalize(dir.path()).unwrap().join("runs"));
    }
}
