//! The agent never runs as root, except inside a container where it only warns.

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootCheck {
    NotRoot,
    RootInContainer,
}

pub fn check() -> Result<RootCheck> {
    if !is_root() {
        return Ok(RootCheck::NotRoot);
    }
    if in_container() {
        tracing::warn!("running as root inside a container; terminal sessions would run as root");
        return Ok(RootCheck::RootInContainer);
    }
    if cfg!(windows) {
        bail!(
            "PocketGPU refuses to run as Administrator, so a paired phone can never control the machine \
             with admin rights, and Windows didn't let it drop them. Run it from a normal (not elevated) \
             terminal."
        )
    }
    bail!(
        "PocketGPU refuses to run as root, so a paired phone can never control the machine as root. \
         Run it as your normal user."
    )
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// On Windows, running elevated (as Administrator) counts as root.
#[cfg(windows)]
fn is_root() -> bool {
    crate::winsys::is_elevated()
}

#[cfg(not(any(unix, windows)))]
fn is_root() -> bool {
    false
}

/// Whether the agent runs inside a container (Docker, Podman, Kubernetes, LXC). Linux only:
/// on Windows `C:\.dockerenv` is a path any user could create, and the agent has no
/// Windows-container build.
#[cfg(not(target_os = "linux"))]
pub fn in_container() -> bool {
    false
}

/// Whether the agent runs inside a container (Docker, Podman, Kubernetes, LXC).
#[cfg(target_os = "linux")]
pub fn in_container() -> bool {
    std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
        || std::fs::read_to_string("/proc/1/cgroup")
            .map(|c| {
                ["docker", "kubepods", "containerd", "lxc"]
                    .iter()
                    .any(|k| c.contains(k))
            })
            .unwrap_or(false)
}
