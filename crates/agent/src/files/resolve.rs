//! A root and a path from a device → a checked path (FR-003). Paths are `/`-separated and
//! relative; `..`, `.`, empty parts, absolute paths, NUL, backslashes and drive prefixes are
//! refused before the disk is touched. The result is resolved (links followed) and must
//! stay inside the root, unless the root lets links lead out (all your files, where they
//! may lead anywhere but PocketGPU's own folders). Files are opened without following a last link or waiting on a
//! pipe, and the open file is compared with the path afterwards, so a swap in between is
//! caught.

use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};

use pocketgpu_proto::{Error, ErrorCode};

/// Why a path was refused.
#[derive(Debug, thiserror::Error)]
pub enum Refused {
    #[error("not a valid path: {0}")]
    BadPath(&'static str),
    #[error("no such file or folder here")]
    NotFound,
    #[error("that path leads outside the folder")]
    Outside,
    #[error("only regular files can be saved")]
    NotAFile,
    #[error("not a folder")]
    NotAFolder,
    #[error("the file changed while it was being opened; try again")]
    Swapped,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl Refused {
    pub fn to_error(&self) -> Error {
        let code = match self {
            Self::BadPath(_) | Self::NotAFile | Self::NotAFolder => ErrorCode::Invalid,
            Self::NotFound => ErrorCode::NotFound,
            Self::Outside => ErrorCode::Forbidden,
            Self::Swapped => ErrorCode::Busy,
            Self::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => ErrorCode::Forbidden,
            Self::Io(_) => ErrorCode::Internal,
        };
        Error::new(code, self.to_string())
    }
}

/// How a root treats paths in it.
#[derive(Debug, Clone)]
pub struct Rules {
    /// Canonical.
    pub root: PathBuf,
    /// Names starting with `.` may be opened (only for "All your files").
    pub hidden: bool,
    /// Links may lead out of the root (only for "All your files", which is everything
    /// the agent's user can read); paths still start at the root.
    pub outside: bool,
    /// Folders that stay closed (PocketGPU's own keys), canonical.
    pub deny: Vec<PathBuf>,
}

impl Rules {
    pub fn new(root: &Path) -> Result<Self, Refused> {
        Ok(Self {
            root: root.canonicalize().map_err(not_found)?,
            hidden: false,
            outside: false,
            deny: Vec::new(),
        })
    }

    /// The checked, canonical path of `rel` (`""` is the root itself).
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, Refused> {
        let mut path = self.root.clone();
        for part in parts(rel)? {
            if !self.hidden && part.starts_with('.') {
                return Err(Refused::NotFound);
            }
            path.push(part);
        }
        let canon = path.canonicalize().map_err(not_found)?;
        self.check(&canon)?;
        Ok(canon)
    }

    /// Whether a canonical path is inside the root (or anywhere, when links may lead
    /// out), not hidden when hidden names are refused, and not in a denied folder.
    pub fn check(&self, canon: &Path) -> Result<(), Refused> {
        if self.deny.iter().any(|d| canon.starts_with(d)) {
            return Err(Refused::Outside);
        }
        let below = match canon.strip_prefix(&self.root) {
            Ok(below) => below,
            Err(_) if self.outside => return Ok(()),
            Err(_) => return Err(Refused::Outside),
        };
        if !self.hidden
            && below
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            return Err(Refused::NotFound);
        }
        Ok(())
    }

    /// A folder to list.
    pub fn folder(&self, rel: &str) -> Result<PathBuf, Refused> {
        let path = self.resolve(rel)?;
        if !path.is_dir() {
            return Err(Refused::NotAFolder);
        }
        Ok(path)
    }

    /// A regular file, opened, with its metadata.
    pub fn open(&self, rel: &str) -> Result<(File, Metadata), Refused> {
        let path = self.resolve(rel)?;
        let (file, meta) = open_regular(&path)?;
        // Still inside after opening, and still the same file.
        let again = path.canonicalize().map_err(|_| Refused::Swapped)?;
        self.check(&again).map_err(|_| Refused::Swapped)?;
        same_file(&meta, &again)?;
        Ok((file, meta))
    }
}

/// Opens exactly this canonical path as a regular file (an offered file): no links on the
/// way, and still the same file afterwards.
pub fn open_exact(path: &Path) -> Result<(File, Metadata), Refused> {
    let canon = path.canonicalize().map_err(not_found)?;
    if canon != path {
        return Err(Refused::Outside);
    }
    let (file, meta) = open_regular(path)?;
    same_file(&meta, path)?;
    Ok((file, meta))
}

/// The parts of a device's relative path, refused when any could leave the root.
pub fn parts(rel: &str) -> Result<Vec<&str>, Refused> {
    if rel.is_empty() {
        return Ok(Vec::new());
    }
    if rel.starts_with('/') {
        return Err(Refused::BadPath("it must be relative"));
    }
    let parts: Vec<&str> = rel.split('/').collect();
    for part in &parts {
        let bad = match *part {
            "" => Some("empty part"),
            "." | ".." => Some("no . or .. parts"),
            p if p.contains('\0') => Some("NUL in a name"),
            p if p.contains('\\') => Some("no backslashes"),
            p if is_drive(p) => Some("no drive letters"),
            // `name:stream` opens another stream of the file on Windows.
            p if cfg!(windows) && p.contains(':') => Some("no colons"),
            _ => None,
        };
        if let Some(why) = bad {
            return Err(Refused::BadPath(why));
        }
    }
    Ok(parts)
}

/// `C:` or `c:anything`.
fn is_drive(part: &str) -> bool {
    let b = part.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn not_found(e: std::io::Error) -> Refused {
    match e.kind() {
        std::io::ErrorKind::NotFound => Refused::NotFound,
        _ => Refused::Io(e),
    }
}

/// Opens a path that should be a regular file: without following a link in its last part
/// and without waiting on a pipe (Unix), and refused when it isn't a regular file.
fn open_regular(path: &Path) -> Result<(File, Metadata), Refused> {
    if !std::fs::symlink_metadata(path)
        .map_err(not_found)?
        .is_file()
    {
        return Err(Refused::NotAFile);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    let file = options.open(path).map_err(not_found)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Refused::NotAFile);
    }
    Ok((file, meta))
}

/// Whether the open file is the one at `path` now.
fn same_file(meta: &Metadata, path: &Path) -> Result<(), Refused> {
    let now = std::fs::metadata(path).map_err(|_| Refused::Swapped)?;
    #[cfg(unix)]
    let same = {
        use std::os::unix::fs::MetadataExt;
        meta.dev() == now.dev() && meta.ino() == now.ino()
    };
    #[cfg(not(unix))]
    let same = meta.len() == now.len() && meta.modified().ok() == now.modified().ok();
    if same { Ok(()) } else { Err(Refused::Swapped) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, Rules) {
        let dir = tempfile::Builder::new()
            .prefix("pgpu-resolve-")
            .tempdir()
            .unwrap();
        let root = dir.path().join("shared");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("sub/b.txt"), "b").unwrap();
        std::fs::write(root.join(".env"), "secret").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "no").unwrap();
        let rules = Rules::new(&root).unwrap();
        (dir, rules)
    }

    fn refused(rules: &Rules, rel: &str) -> String {
        match rules.open(rel) {
            Err(e) => format!("{e:?}"),
            Ok(_) => "opened".into(),
        }
    }

    #[test]
    fn opens_files_inside_the_root() {
        let (_dir, rules) = tree();
        assert_eq!(rules.open("a.txt").unwrap().1.len(), 1);
        assert!(rules.open("sub/b.txt").is_ok());
        assert_eq!(rules.folder("").unwrap(), rules.root);
        assert!(rules.folder("sub").is_ok());
        assert!(matches!(rules.folder("a.txt"), Err(Refused::NotAFolder)));
        assert!(matches!(rules.open("sub"), Err(Refused::NotAFile)));
        assert!(matches!(rules.open("missing"), Err(Refused::NotFound)));
    }

    #[test]
    fn refuses_paths_that_could_leave() {
        let (_dir, rules) = tree();
        for bad in [
            "../outside.txt",
            "sub/../../outside.txt",
            "/etc/passwd",
            "./a.txt",
            "sub//b.txt",
            "a.txt/",
            "C:/Windows",
            "c:x",
            "sub\\b.txt",
            "a\0.txt",
        ] {
            assert!(refused(&rules, bad).starts_with("BadPath"), "{bad}");
        }
    }

    #[test]
    fn hidden_names_are_skipped_unless_allowed() {
        let (_dir, mut rules) = tree();
        assert!(matches!(rules.open(".env"), Err(Refused::NotFound)));
        rules.hidden = true;
        assert!(rules.open(".env").is_ok());
    }

    #[test]
    fn denied_folders_stay_closed() {
        let (_dir, mut rules) = tree();
        rules.deny = vec![rules.root.join("sub")];
        assert!(matches!(rules.open("sub/b.txt"), Err(Refused::Outside)));
        assert!(rules.open("a.txt").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_the_root_are_refused() {
        let (dir, rules) = tree();
        let link = rules.root.join("escape.txt");
        std::os::unix::fs::symlink(dir.path().join("outside.txt"), &link).unwrap();
        assert!(matches!(rules.open("escape.txt"), Err(Refused::Outside)));
        std::os::unix::fs::symlink(dir.path(), rules.root.join("up")).unwrap();
        assert!(matches!(rules.folder("up"), Err(Refused::Outside)));
        // A link to a hidden file inside is hidden too.
        std::os::unix::fs::symlink(rules.root.join(".env"), rules.root.join("env")).unwrap();
        assert!(matches!(rules.open("env"), Err(Refused::NotFound)));
        // A link that stays inside is fine.
        std::os::unix::fs::symlink(rules.root.join("a.txt"), rules.root.join("same")).unwrap();
        assert!(rules.open("same").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn all_your_files_follows_links_out_but_not_into_pocketgpu() {
        let (dir, mut rules) = tree();
        let own = dir.path().join("pgpu-data");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("key"), "secret").unwrap();
        std::os::unix::fs::symlink(dir.path(), rules.root.join("disk")).unwrap();
        std::os::unix::fs::symlink(&own, rules.root.join("own")).unwrap();
        rules.hidden = true;
        rules.outside = true;
        rules.deny = vec![own.canonicalize().unwrap()];
        assert!(rules.open("disk/outside.txt").is_ok());
        assert!(rules.folder("disk").is_ok());
        assert!(matches!(rules.open("own/key"), Err(Refused::Outside)));
        assert!(matches!(
            rules.open("disk/pgpu-data/key"),
            Err(Refused::Outside)
        ));
        // Paths themselves still can't climb out.
        assert!(matches!(
            rules.open("../outside.txt"),
            Err(Refused::BadPath(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pipes_and_devices_are_not_files() {
        let (_dir, rules) = tree();
        let fifo = rules.root.join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(matches!(rules.open("pipe"), Err(Refused::NotAFile)));
        let dev = Rules::new(Path::new("/dev")).unwrap();
        assert!(matches!(dev.open("null"), Err(Refused::NotAFile)));
    }

    #[test]
    fn offered_files_open_only_at_their_exact_path() {
        let (_dir, rules) = tree();
        let file = rules.root.join("a.txt");
        assert!(open_exact(&file).is_ok());
        // Any other spelling of the same file is refused. It's built as text because on
        // Windows joining onto a verbatim `\\?\` root removes `..`, giving the exact path.
        // Windows may refuse it in the file system rather than as not exact.
        let mut dotted = rules.root.clone().into_os_string();
        dotted.push(format!("{0}sub{0}..{0}a.txt", std::path::MAIN_SEPARATOR));
        let dotted = PathBuf::from(dotted);
        assert!(open_exact(&dotted).is_err());
        #[cfg(unix)]
        assert!(matches!(open_exact(&dotted), Err(Refused::Outside)));
        assert!(matches!(
            open_exact(&rules.root.join("gone.txt")),
            Err(Refused::NotFound)
        ));
    }
}
