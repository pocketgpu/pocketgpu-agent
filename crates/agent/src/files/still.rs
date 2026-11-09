//! A transfer goes on only while it could still start (FR-002): files on, the device's
//! permission still high enough for the root, the terminal still on for all your files,
//! and a shared folder still shared. Files on is checked before every chunk, the rest once
//! a second, so `pgpu files off`, `pgpu shell off`, `pgpu files unshare` and a lowered or
//! revoked permission stop the transfers already running, as `pgpu shell off` closes
//! terminals.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iroh::PublicKey;
use pocketgpu_proto::files::Root;
use pocketgpu_proto::{Error, ErrorCode, Permission};

use crate::daemon::state::Agent;

/// How often the checks that read the database or the system run during a transfer.
pub const EVERY: Duration = Duration::from_secs(1);

/// What one transfer needs to go on.
#[derive(Debug, Clone)]
pub struct Still {
    key: PublicKey,
    needed: Permission,
    /// All your files: the terminal must stay on.
    home: bool,
    /// The shared folder a `Get` reads from, canonical: it must stay shared.
    shared: Option<PathBuf>,
    checked: Instant,
}

impl Still {
    /// A `Get` from `root`; `shared` is the folder of a `Root::Shared`.
    pub fn get(key: PublicKey, root: Root, shared: Option<PathBuf>) -> Self {
        Self {
            key,
            needed: root.min_permission(),
            home: root == Root::Home,
            shared,
            checked: Instant::now(),
        }
    }

    /// A `Put` into the Inbox.
    pub fn put(key: PublicKey) -> Self {
        Self {
            key,
            needed: Permission::Control,
            home: false,
            shared: None,
            checked: Instant::now(),
        }
    }

    /// Why the transfer must stop now, or `None` while it may go on. Call before each chunk.
    pub fn stopped(&mut self, agent: &Agent) -> Option<Error> {
        if !super::handler::enabled(agent) {
            return Some(super::handler::off_error());
        }
        if self.checked.elapsed() < EVERY {
            return None;
        }
        self.checked = Instant::now();
        self.check(agent)
    }

    /// Every check now, whenever it last ran.
    pub fn check(&self, agent: &Agent) -> Option<Error> {
        match agent.permission_of(&self.key) {
            Some(have) if have >= self.needed => {}
            Some(_) => return Some(Error::forbidden(self.needed)),
            None => {
                return Some(Error::new(
                    ErrorCode::Forbidden,
                    "this device is no longer paired with this machine",
                ));
            }
        }
        if self.home
            && let Err(why) = super::terminal_on(agent)
        {
            return Some(Error::new(ErrorCode::Forbidden, why));
        }
        if let Some(dir) = &self.shared {
            let shared = agent
                .config
                .read()
                .expect("config lock poisoned")
                .files_shared
                .contains(dir);
            if !shared {
                return Some(Error::new(
                    ErrorCode::NotFound,
                    "that folder is no longer shared",
                ));
            }
        }
        None
    }
}
