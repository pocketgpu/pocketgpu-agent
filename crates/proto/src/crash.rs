//! Crash reports (board 25), shared by the app and the agent: a panic hook that writes what
//! went wrong to a file, and reading that file back on the next start to send it to the push
//! server (`POST /v1/crash`). A report holds the error, where in the code it stopped, the
//! version, the system and (for phones) the model: never names, commands or logs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A crash this soon after start is a crash on start.
const ON_START: Duration = Duration::from_secs(20);
const FRAMES: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReport {
    /// The install's random number (the app's ping number; the agent keeps its own).
    pub install: String,
    /// `app` or `agent`.
    pub source: String,
    /// `android`, `ios`, `windows`, `macos` or `linux`.
    pub platform: String,
    pub version: String,
    pub os: Option<String>,
    pub model: Option<String>,
    pub message: String,
    pub frames: Vec<String>,
    pub on_start: bool,
}

/// This build's platform name, as the push server counts them.
pub fn platform() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Our own frames from a backtrace's text, newest first: the function and, when known, the
/// file and line. Frames of the standard library and of the panic itself are left out.
pub fn frames(backtrace: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut lines = backtrace.lines().map(str::trim).peekable();
    while let Some(line) = lines.next() {
        // "12: pocketgpu::daemon::run::{{closure}}" then optionally "at src/daemon/mod.rs:40:5".
        let Some((n, name)) = line.split_once(": ") else {
            continue;
        };
        if n.parse::<u32>().is_err() {
            continue;
        }
        let at = lines
            .peek()
            .and_then(|l| l.strip_prefix("at "))
            .map(|l| l.rsplit(['/', '\\']).next().unwrap_or(l).to_string());
        if at.is_some() {
            lines.next();
        }
        let skip = [
            "std::",
            "core::",
            "alloc::",
            "rust_begin_unwind",
            "__rust",
            "<alloc::",
            "<core::",
            "<std::",
            "pocketgpu_proto::crash",
        ]
        .iter()
        .any(|p| name.starts_with(p))
            || name.starts_with("__")
            || name.contains("panicking");
        if skip {
            continue;
        }
        let name = name.split("::h").next().unwrap_or(name);
        out.push(match at {
            Some(at) => format!("{name}  {at}"),
            None => name.to_string(),
        });
        if out.len() == FRAMES {
            break;
        }
    }
    out
}

/// Writes a report to `pending` when the program panics, then lets the previous hook run.
/// `base` is everything but the message, frames and when.
pub fn install_hook(pending: PathBuf, base: CrashReport) {
    let started = Instant::now();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = match info.payload() {
            p if p.is::<&str>() => p.downcast_ref::<&str>().map(|s| s.to_string()),
            p => p.downcast_ref::<String>().cloned(),
        }
        .unwrap_or_else(|| "panic".into());
        let mut frames = Vec::new();
        if let Some(l) = info.location() {
            let file = l.file().rsplit(['/', '\\']).next().unwrap_or(l.file());
            frames.push(format!("panicked at {file}:{}", l.line()));
        }
        frames.extend(self::frames(
            &std::backtrace::Backtrace::force_capture().to_string(),
        ));
        frames.truncate(FRAMES);
        let report = CrashReport {
            message,
            frames,
            on_start: started.elapsed() < ON_START,
            ..base.clone()
        };
        let _ = save(&pending, &report);
        previous(info);
    }));
}

/// Saves a report to send later (for errors that end the program without a panic).
pub fn save(pending: &Path, report: &CrashReport) -> std::io::Result<()> {
    let tmp = pending.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(report).unwrap_or_default())?;
    std::fs::rename(tmp, pending)
}

/// The report the last run left, if any; the file is removed either way.
pub fn take(pending: &Path) -> Option<CrashReport> {
    let bytes = std::fs::read(pending).ok()?;
    let _ = std::fs::remove_file(pending);
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_our_frames_with_their_files() {
        let bt = "   0: std::backtrace::Backtrace::force_capture
             at /rustc/abc/library/std/src/backtrace.rs:312:13
   1: pocketgpu_proto::crash::install_hook::{{closure}}
   2: std::panicking::rust_panic_with_hook
   3: pocketgpu_app::live::watch::h1234abcd
             at /home/ann/src/pocketgpu/app/src-tauri/src/live.rs:88:9
   4: tauri::app::Builder::build
   5: core::ops::function::FnOnce::call_once";
        assert_eq!(
            frames(bt),
            [
                "pocketgpu_app::live::watch  live.rs:88:9",
                "tauri::app::Builder::build"
            ]
        );
    }

    #[test]
    fn a_saved_report_is_taken_once() {
        let dir = std::env::temp_dir().join(format!("pgpu-crash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pending = dir.join("crash.json");
        let r = CrashReport {
            install: "a".repeat(32),
            source: "agent".into(),
            platform: platform().into(),
            version: "1.2.4".into(),
            os: None,
            model: None,
            message: "boom".into(),
            frames: vec![],
            on_start: false,
        };
        save(&pending, &r).unwrap();
        assert_eq!(take(&pending), Some(r));
        assert_eq!(take(&pending), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
