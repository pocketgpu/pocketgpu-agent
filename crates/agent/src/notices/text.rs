//! The words of each notice.

use super::{Kind, Notice, Remote};

/// How to stop a power countdown from the notice: its button, or on macOS the command line.
#[cfg(not(target_os = "macos"))]
const CANCEL_HINT: &str = "To stop it, press Cancel.";
#[cfg(target_os = "macos")]
const CANCEL_HINT: &str = "To stop it, run: pgpu power cancel";

/// How to revoke from the notice: its button, or on macOS (no buttons yet) the app.
#[cfg(not(target_os = "macos"))]
const REVOKE_HINT: &str = "Revoke it.";
#[cfg(target_os = "macos")]
const REVOKE_HINT: &str = "Revoke it in PocketGPU, or with: pgpu devices revoke";

/// The notice for one action.
pub fn text(r: &Remote, machine: &str) -> Notice {
    let who = &r.name;
    let (title, body) = match r.kind {
        // A guest (F44): "control · guest for 8 hours · from Sam's iPhone".
        Kind::Pair if r.result.contains(" · guest ") => {
            let mut parts = r.result.split(" · ");
            let level = match parts.next().unwrap_or("view") {
                "shell" => "Full control",
                "control" => "Can run actions",
                _ => "Can only look",
            };
            let how_long = parts.next().unwrap_or("guest").trim_start_matches("guest ");
            let by = parts
                .next()
                .map_or("someone", |b| b.trim_start_matches("from "));
            (
                format!("{who} joined as a guest"),
                format!(
                    "Invited by {by}: {level}, {how_long}. It ends by itself. Not you? {REVOKE_HINT}"
                ),
            )
        }
        Kind::Pair => {
            let perm = r.result.split(' ').next().unwrap_or("view");
            let what = match perm {
                "shell" => "Shell access, so it can open a terminal here",
                "control" => "Control access, so it can stop processes and run actions here",
                _ => "View access, so it can see this computer's stats",
            };
            (
                format!("New device paired: {who}"),
                format!("With {what}. Not you? {REVOKE_HINT}"),
            )
        }
        Kind::Terminal => (
            format!("{who} opened a terminal"),
            format!("On {machine}, as you. PocketGPU's tray shows who is connected."),
        ),
        Kind::Kill => (
            format!("{who} stopped {}", r.target),
            format!("On {machine}."),
        ),
        Kind::JobStop => (
            format!("{who} stopped {}", r.target),
            format!("On {machine}."),
        ),
        Kind::Action => (
            format!("{who} ran {}", action_name(&r.target)),
            format!("On {machine} · {}.", r.result),
        ),
        Kind::Container => (
            format!("{who} {}", container_text(&r.target)),
            format!("On {machine}."),
        ),
        Kind::Revoke => (
            format!("{who} revoked {}", r.target),
            format!("{} can no longer connect to {machine}.", r.target),
        ),
        Kind::Power => {
            let what = match r.target.as_str() {
                "restart" => "restarting this computer",
                "sleep" => "putting this computer to sleep",
                _ => "shutting down this computer",
            };
            (
                format!("{who} is {what} in 30 seconds"),
                format!("Save your work. {CANCEL_HINT}"),
            )
        }
        Kind::FileSent => (
            format!("{who} sent a file to this PC"),
            format!("{} is in the Inbox on {machine}.", r.target),
        ),
        Kind::FileTaken => (
            format!("{who} saved a file from this PC"),
            format!("{}, from {machine}.", r.target),
        ),
        Kind::ClipRead => (
            format!("{who} read the clipboard"),
            format!("On {machine}."),
        ),
        Kind::AppOpen => (
            format!("{who} opened {}", app_name(&r.target)),
            format!("On {machine}. It can use the app as you do here."),
        ),
        Kind::GuestEnded => (
            format!("{who}’s access ended"),
            format!("Its time on {machine} was up; it can no longer connect."),
        ),
        Kind::ScreenLook => (
            format!("{who} is looking at this screen"),
            "Looking only. Stop it from PocketGPU's tray, or: pgpu screen stop".to_string(),
        ),
    };
    Notice {
        title,
        body,
        revoke: (r.kind == Kind::Pair).then(|| (r.device.clone(), r.name.clone())),
        cancel_power: r.kind == Kind::Power,
    }
}

/// Several actions of one kind from one device in a minute.
pub fn summary(r: &Remote, count: u32, machine: &str) -> Notice {
    let who = &r.name;
    let first = match r.kind {
        Kind::Action => action_name(&r.target).to_string(),
        Kind::AppOpen => app_name(&r.target).to_string(),
        Kind::Container => r
            .target
            .split_once(' ')
            .map_or(r.target.clone(), |(_, id)| id.into()),
        _ => r.target.clone(),
    };
    let (verb, noun) = match r.kind {
        Kind::Kill => ("stopped", "processes"),
        Kind::JobStop => ("stopped", "jobs"),
        Kind::Action => ("ran", "quick actions"),
        Kind::Container => ("changed", "containers"),
        Kind::Revoke => ("revoked", "devices"),
        Kind::FileSent => ("sent", "files to this PC"),
        Kind::FileTaken => ("saved", "files from this PC"),
        Kind::AppOpen => ("opened", "apps"),
        Kind::ClipRead => {
            return Notice {
                title: format!("{who} read the clipboard {count} times"),
                body: format!("On {machine}, in the last minute."),
                revoke: None,
                cancel_power: false,
            };
        }
        Kind::Pair | Kind::Terminal | Kind::Power => ("used", "things"),
        Kind::GuestEnded => ("ended", "guests’ access"),
        Kind::ScreenLook => ("looked at", "the screen"),
    };
    Notice {
        title: format!("{who} {verb} {count} {noun}"),
        body: format!(
            "{first} and {} more on {machine}, in the last minute.",
            count - 1
        ),
        revoke: None,
        cancel_power: false,
    }
}

/// `Jupyter Lab :8888` → `Jupyter Lab`.
fn app_name(target: &str) -> &str {
    target.rsplit_once(" :").map_or(target, |(name, _)| name)
}

/// `#3 Restart vLLM` → `Restart vLLM`.
fn action_name(target: &str) -> &str {
    match target.split_once(' ') {
        Some((id, name)) if id.starts_with('#') => name,
        _ => target,
    }
}

/// `restart 3f2a…` → `restarted container 3f2a…`.
fn container_text(target: &str) -> String {
    let (action, id) = target.split_once(' ').unwrap_or(("changed", target));
    let short: String = id.chars().take(12).collect();
    let verb = match action {
        "start" => "started",
        "stop" => "stopped",
        "restart" => "restarted",
        other => other,
    };
    format!("{verb} container {short}")
}
