//! Quick actions (F10): saved one-tap commands, run as the agent user.
//!
//! Control may run actions; only Shell (or the command line) may create, edit or delete
//! them, because a saved command can run anything.

pub mod handler;
pub mod local;
pub mod runner;
pub mod store;

use std::collections::HashSet;
use std::sync::Mutex;

use pocketgpu_proto::msg::control::NewAction;
use thiserror::Error;

/// Default and longest run time (F10: default 10 minutes).
pub const DEFAULT_TIMEOUT_S: u32 = 600;
pub const MAX_TIMEOUT_S: u32 = 24 * 3600;
pub const MAX_NAME: usize = 40;
/// Longest command; actions are one-liners, longer scripts belong in a file.
pub const MAX_COMMAND: usize = 4096;

/// Icons the phone can show (Lucide names), matching screen 7.2.
pub const ICONS: &[&str] = &[
    // The six in the 7.2 picker first, then extras the command line can use.
    "refresh-cw",
    "play",
    "rotate-ccw",
    "download",
    "trash-2",
    "power",
    "zap",
    "upload",
    "database",
    "server",
    "terminal",
    "hard-drive",
    "cpu",
    "box",
    "flame",
    "brush-cleaning",
];
/// The icon for actions added without one.
pub const DEFAULT_ICON: &str = "zap";

#[derive(Debug, Error, PartialEq)]
pub enum Invalid {
    #[error("give the action a name")]
    NoName,
    #[error("the name is longer than {MAX_NAME} characters")]
    NameTooLong,
    #[error("an action named \"{0}\" already exists")]
    Duplicate(String),
    #[error("give the action a command")]
    NoCommand,
    #[error("the command is longer than {MAX_COMMAND} characters")]
    CommandTooLong,
    #[error("unknown icon \"{0}\"; use one of: {list}", list = ICONS.join(", "))]
    Icon(String),
    #[error("the timeout must be between 1 second and 24 hours")]
    Timeout,
}

/// A checked action, ready to store.
#[derive(Debug, Clone, PartialEq)]
pub struct Valid {
    pub name: String,
    pub command: String,
    pub icon: String,
    pub risky: bool,
    pub timeout_s: u32,
}

/// Checks and tidies an action from a phone or the command line.
pub fn validate(a: &NewAction) -> Result<Valid, Invalid> {
    let name = a.name.trim().to_string();
    if name.is_empty() {
        return Err(Invalid::NoName);
    }
    if name.chars().count() > MAX_NAME {
        return Err(Invalid::NameTooLong);
    }
    let command = a.command.trim().to_string();
    if command.is_empty() {
        return Err(Invalid::NoCommand);
    }
    if command.chars().count() > MAX_COMMAND {
        return Err(Invalid::CommandTooLong);
    }
    let icon = if a.icon.trim().is_empty() {
        DEFAULT_ICON.to_string()
    } else {
        a.icon.trim().to_string()
    };
    if !ICONS.contains(&icon.as_str()) {
        return Err(Invalid::Icon(icon));
    }
    let timeout_s = a.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S);
    if !(1..=MAX_TIMEOUT_S).contains(&timeout_s) {
        return Err(Invalid::Timeout);
    }
    Ok(Valid {
        name,
        command,
        icon,
        risky: a.risky,
        timeout_s,
    })
}

/// Actions running right now, so the same action isn't started twice at once.
#[derive(Debug, Default)]
pub struct Running(Mutex<HashSet<u64>>);

impl Running {
    /// Marks `id` as running; `None` if it already is. The guard clears it when dropped.
    pub fn start(&self, id: u64) -> Option<RunGuard<'_>> {
        let mut set = self.0.lock().expect("running lock poisoned");
        // Not `then_some`: it would build (and drop) a guard while the lock is held.
        if set.insert(id) {
            Some(RunGuard { running: self, id })
        } else {
            None
        }
    }
}

pub struct RunGuard<'a> {
    running: &'a Running,
    id: u64,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.running.0.lock() {
            set.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(name: &str, command: &str, icon: &str) -> NewAction {
        NewAction {
            name: name.into(),
            command: command.into(),
            icon: icon.into(),
            risky: false,
            timeout_s: None,
        }
    }

    #[test]
    fn validates_and_fills_defaults() {
        let v = validate(&new(
            "  Restart vLLM ",
            " systemctl --user restart vllm ",
            "",
        ))
        .unwrap();
        assert_eq!(v.name, "Restart vLLM");
        assert_eq!(v.command, "systemctl --user restart vllm");
        assert_eq!(v.icon, "zap");
        assert_eq!(v.timeout_s, DEFAULT_TIMEOUT_S);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(validate(&new("", "true", "zap")), Err(Invalid::NoName));
        assert_eq!(
            validate(&new(&"x".repeat(41), "true", "zap")),
            Err(Invalid::NameTooLong)
        );
        assert_eq!(validate(&new("A", " ", "zap")), Err(Invalid::NoCommand));
        assert_eq!(
            validate(&new("A", "true", "rocket")),
            Err(Invalid::Icon("rocket".into()))
        );
        let mut slow = new("A", "true", "zap");
        slow.timeout_s = Some(0);
        assert_eq!(validate(&slow), Err(Invalid::Timeout));
    }

    #[test]
    fn the_same_action_runs_once_at_a_time() {
        let r = Running::default();
        let g = r.start(1).unwrap();
        assert!(r.start(1).is_none());
        assert!(r.start(2).is_some());
        drop(g);
        assert!(r.start(1).is_some());
    }
}
