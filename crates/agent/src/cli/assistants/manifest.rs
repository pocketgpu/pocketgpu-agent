//! The list of changes connect made, in the agent's data folder, and taking them back.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::files;
use super::jsonedit;
use super::places::{Places, Runner};
use super::plan::{Change, FileUndo, Record};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub changes: Vec<Record>,
    /// Assistant id → why "Ask me first" can't work for it here (F32), from the last connect
    /// or `pgpu assistants`; the agent reads it ([`crate::assistants::blocked`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub check_blocked: BTreeMap<String, String>,
}

impl Manifest {
    pub fn load(places: &Places) -> Result<Self> {
        match files::read(&places.manifest())? {
            Some(text) => serde_json::from_str(&text)
                .with_context(|| format!("could not read {}", places.manifest().display())),
            None => Ok(Self::default()),
        }
    }

    /// Saved after every change, so a failure halfway still leaves an accurate list.
    pub fn save(&self, places: &Places) -> Result<()> {
        let path = places.manifest();
        if self.changes.is_empty() && self.check_blocked.is_empty() {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir(places.backups());
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir(dir);
            }
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            files::make_dirs(dir)?;
        }
        let text = serde_json::to_string_pretty(&Manifest {
            version: 1,
            changes: self.changes.clone(),
            check_blocked: self.check_blocked.clone(),
        })?;
        crate::store::paths::write_private(&path, text.as_bytes())
    }

    /// Records why "Ask me first" can't work for `assistant` (or that it can), saving only
    /// when that changed.
    pub fn record_blocked(places: &Places, assistant: &str, why: Option<String>) -> Result<()> {
        let mut m = Self::load(places)?;
        let before = m.check_blocked.get(assistant).cloned();
        if before == why {
            return Ok(());
        }
        match why {
            Some(why) => m.check_blocked.insert(assistant.to_string(), why),
            None => m.check_blocked.remove(assistant),
        };
        m.save(places)
    }

    /// Whether connect wrote this file and it still holds exactly that.
    pub fn wrote(&self, path: &Path, text: &str) -> bool {
        let hash = files::hash(text);
        self.changes.iter().any(|r| {
            matches!(&r.change, Change::File { path: p, written, .. } if p == path && *written == hash)
        })
    }
}

/// How taking one change back went.
#[derive(Debug, PartialEq)]
pub enum Undone {
    Done,
    /// Left in place, and why, with what to do about it.
    Left(String),
}

/// Takes back one change. A file that is as connect left it gets its old copy back byte for
/// byte; one changed since loses only what connect added.
pub fn undo(record: &Record, places: &Places, runner: &dyn Runner) -> Result<Undone> {
    match &record.change {
        Change::Command { undo } => {
            let Some((name, args)) = undo.split_first() else {
                return Ok(Undone::Done);
            };
            let Some(program) = runner.find(name) else {
                return Ok(Undone::Left(format!(
                    "{name} isn't found; remove it with: {}",
                    undo.join(" ")
                )));
            };
            let out = runner.run(&program, args)?;
            if out.ok {
                Ok(Undone::Done)
            } else {
                Ok(Undone::Left(format!(
                    "{} failed: {}",
                    undo.join(" "),
                    out.stderr.trim().lines().next().unwrap_or("")
                )))
            }
        }
        Change::File {
            path,
            backup,
            written,
            made_dirs,
            undo,
        } => {
            let Some(now) = files::read(path)? else {
                files::remove_dirs(made_dirs);
                return Ok(Undone::Done);
            };
            if files::hash(&now) == *written {
                match backup {
                    Some(b) => {
                        let old = files::read(b)?.context("the backup is missing")?;
                        files::write(path, &old)?;
                        let _ = std::fs::remove_file(b);
                    }
                    None => {
                        std::fs::remove_file(path)
                            .with_context(|| format!("could not remove {}", path.display()))?;
                        files::remove_dirs(made_dirs);
                    }
                }
                return Ok(Undone::Done);
            }
            let Some(text) = take_out(&now, undo)? else {
                return Ok(Undone::Left(format!(
                    "{} changed since, so it was left as is",
                    places.show(path)
                )));
            };
            if backup.is_none() && is_leftover(&text) {
                std::fs::remove_file(path)
                    .with_context(|| format!("could not remove {}", path.display()))?;
                files::remove_dirs(made_dirs);
            } else if text != now {
                files::write(path, &text)?;
            }
            if let Some(b) = backup {
                let _ = std::fs::remove_file(b);
            }
            Ok(Undone::Done)
        }
    }
}

/// The text without what connect added; `None` when that can't be done safely.
fn take_out(text: &str, undo: &FileUndo) -> Result<Option<String>> {
    Ok(Some(match undo {
        FileUndo::Delete => return Ok(None),
        FileUndo::Appended { text: ours } => match text.rfind(ours.as_str()) {
            Some(i) => format!("{}{}", &text[..i], &text[i + ours.len()..]),
            None => return Ok(None),
        },
        FileUndo::Member { at, key, made } => {
            let at: Vec<&str> = at.iter().map(String::as_str).collect();
            let text = jsonedit::remove(text, &at, key)?;
            prune(&text, made)?
        }
        FileUndo::Hooks { lists, hook, made } => {
            let mut text = text.to_string();
            for list in lists {
                let list: Vec<&str> = list.iter().map(String::as_str).collect();
                text = remove_hook(&text, &list, hook)?;
            }
            prune(&text, made)?
        }
    }))
}

/// Same command (and arguments, in exec form) as the hook connect wrote.
pub fn same_hook(a: &Value, hook: &Value) -> bool {
    a.get("command").is_some()
        && a.get("command") == hook.get("command")
        && a.get("args") == hook.get("args")
}

fn remove_hook(text: &str, list: &[&str], hook: &Value) -> Result<String> {
    // Cursor: the list holds the hooks themselves.
    let text = jsonedit::remove_items(text, list, |v| same_hook(v, hook))?;
    // Claude Code and Codex: a group holding only our hook goes whole.
    let only_ours = |g: &Value| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hs| !hs.is_empty() && hs.iter().all(|h| same_hook(h, hook)))
    };
    let mut text = jsonedit::remove_items(&text, list, only_ours)?;
    // Someone added their own hook to our group: take out just ours.
    let groups = jsonedit::get(&text, list)?.unwrap_or(Value::Null);
    for (i, group) in groups.as_array().into_iter().flatten().enumerate() {
        let ours = group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hs| hs.iter().any(|h| same_hook(h, hook)));
        if ours {
            let index = i.to_string();
            let mut inner = list.to_vec();
            inner.extend([index.as_str(), "hooks"]);
            text = jsonedit::remove_items(&text, &inner, |h| same_hook(h, hook))?;
        }
    }
    Ok(text)
}

/// Removes the objects and lists connect created on the way, when they are empty now.
fn prune(text: &str, made: &[Vec<String>]) -> Result<String> {
    let mut made: Vec<&Vec<String>> = made.iter().collect();
    made.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut text = text.to_string();
    for path in made {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        let Some((key, at)) = path.split_last() else {
            continue;
        };
        let empty = match jsonedit::get(&text, &path)? {
            Some(Value::Object(m)) => m.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            _ => false,
        };
        if empty {
            text = jsonedit::remove(&text, at, key)?;
        }
    }
    Ok(text)
}

/// A file connect made that now holds nothing else: `{}`, or Cursor's `{"version": 1}`.
fn is_leftover(text: &str) -> bool {
    match jsonedit::read(text) {
        Ok(Value::Object(m)) => m.keys().all(|k| k == "version"),
        _ => text.trim().is_empty(),
    }
}
