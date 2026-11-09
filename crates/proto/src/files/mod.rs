//! `pgpu/files/1` (F40): files between paired devices and the machine. One request per
//! bidirectional stream. `Overview` and `List` get one reply; `Get` gets `Head`, `Chunk`
//! frames and `Done`; `Put` gets `Ready`, then the device sends `Upload::Chunk` frames and
//! `Upload::Done` with the file's hash, and the agent ends with `Saved` once the hash
//! matches. Off until `pgpu files on` at the machine.

use serde::{Deserialize, Serialize};

use crate::{Error, Permission};

mod name;

pub use name::clean_name;

/// Bytes per `Chunk` frame, both ways.
pub const CHUNK: usize = 256 * 1024;
/// On the default relays, bigger files wait for a direct link (FR-007).
pub const RELAY_LIMIT: u64 = 50 * 1024 * 1024;
/// Entries per `Entries` reply at most.
pub const PAGE: u32 = 500;
/// Longest file name the agent saves, in bytes.
pub const MAX_NAME: usize = 200;
/// Offers in an `Overview` at most, the newest; `List { root: Offers }` pages them all.
pub const OVERVIEW_OFFERS: usize = 50;

/// Where a path starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Root {
    /// `Overview.shared[i]`.
    Shared(u32),
    /// Where files sent to this machine land.
    Inbox,
    /// The logs of `pgpu run` jobs.
    JobLogs,
    /// Files offered with `pgpu send`; the path is the offer's id.
    Offers,
    /// The agent's user's home folder, and wherever links in it lead (PocketGPU's own
    /// folders aside), hidden files included: Shell, and only while the terminal is on.
    Home,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// What this device may open here.
    Overview,
    /// One page of a folder: `path` is `/`-separated and relative to the root, `""` for the
    /// root itself; `offset` is where the page starts (`Entries.next` of the one before).
    List {
        root: Root,
        path: String,
        offset: u32,
    },
    /// A file's bytes from `from` on (0 for all of it, more to resume).
    Get { root: Root, path: String, from: u64 },
    /// Sends a file into the Inbox. `transfer` is chosen by the device and stays the same
    /// when it resumes, so the agent finds the partial file again.
    Put {
        name: String,
        size: u64,
        transfer: [u8; 16],
    },
}

/// What the device sends after `Ready`: chunks until `size` bytes in all, then `Done`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Upload {
    /// At most [`CHUNK`] bytes.
    Chunk(Vec<u8>),
    /// The BLAKE3 hash of the whole file as the device has it. The agent saves the file
    /// only when what it received has this hash; otherwise it drops what it has, so the
    /// next try starts over.
    Done { blake3: [u8; 32] },
}

/// A shared folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// The last part of its path, such as `output`.
    pub name: String,
    /// As people know it, such as `~/ComfyUI/output`.
    pub path: String,
    /// Files directly inside it.
    pub files: u32,
}

/// A file offered with `pgpu send`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub id: String,
    pub name: String,
    pub size: u64,
    /// Unix milliseconds.
    pub offered_ms: u64,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub enabled: bool,
    pub shared: Vec<Folder>,
    /// Where the Inbox is, as people know it (`~/PocketGPU/Inbox`).
    pub inbox: String,
    pub inbox_files: u32,
    /// Job logs this device may read.
    pub job_logs: u32,
    /// The newest offers for this device, at most [`OVERVIEW_OFFERS`].
    pub offers: Vec<Offer>,
    /// "All your files": this device has Shell and the terminal is on.
    pub home: bool,
    /// Files bigger than this wait for a direct link; `None` on custom relays.
    pub relay_limit: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    /// Bytes; 0 for folders.
    pub size: u64,
    /// Unix milliseconds.
    pub modified_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Overview(Overview),
    /// Folders first, then newest first. `next` is the offset of the next page, if any.
    Entries {
        entries: Vec<Entry>,
        next: Option<u32>,
    },
    /// The start of a `Get`: `Chunk` frames from `from` follow, then `Done`.
    Head {
        size: u64,
        modified_ms: u64,
    },
    Chunk(Vec<u8>),
    /// The BLAKE3 hash of the whole file, not only the part sent.
    Done {
        blake3: [u8; 32],
    },
    /// The answer to `Put`: bytes already here for this device and transfer; the device
    /// sends the rest.
    Ready {
        have: u64,
    },
    /// The file is in the Inbox under `name`. Also the answer to a `Put` the agent saved
    /// already (the device missed this reply): `Ready { have: size }`, the device's
    /// `Upload::Done`, then this, without saving it twice.
    Saved {
        name: String,
        blake3: [u8; 32],
    },
    Error(Error),
}

impl Root {
    /// Job logs need View; shared folders, the Inbox and offers Control; everything else
    /// Shell (and the terminal switch, checked by the agent).
    pub fn min_permission(self) -> Permission {
        match self {
            Self::JobLogs => Permission::View,
            Self::Shared(_) | Self::Inbox | Self::Offers => Permission::Control,
            Self::Home => Permission::Shell,
        }
    }
}

impl Request {
    pub fn min_permission(&self) -> Permission {
        match self {
            Self::Overview => Permission::View,
            Self::List { root, .. } | Self::Get { root, .. } => root.min_permission(),
            Self::Put { .. } => Permission::Control,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_follow_the_root() {
        let get = |root| Request::Get {
            root,
            path: "a".into(),
            from: 0,
        };
        assert_eq!(Request::Overview.min_permission(), Permission::View);
        assert_eq!(get(Root::JobLogs).min_permission(), Permission::View);
        assert_eq!(get(Root::Shared(0)).min_permission(), Permission::Control);
        assert_eq!(get(Root::Inbox).min_permission(), Permission::Control);
        assert_eq!(get(Root::Offers).min_permission(), Permission::Control);
        assert_eq!(get(Root::Home).min_permission(), Permission::Shell);
        let list = Request::List {
            root: Root::Home,
            path: String::new(),
            offset: 0,
        };
        assert_eq!(list.min_permission(), Permission::Shell);
        let put = Request::Put {
            name: "a".into(),
            size: 1,
            transfer: [0; 16],
        };
        assert_eq!(put.min_permission(), Permission::Control);
    }
}
