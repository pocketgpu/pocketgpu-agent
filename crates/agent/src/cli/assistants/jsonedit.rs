//! Edits to an assistant's JSON settings that keep everything else as the user had it: key
//! order, spacing and comments. serde_json here doesn't keep key order (no `preserve_order`
//! in this workspace), so parsing and writing a file back would shuffle someone's settings.
//! These edits find the place in the text and splice there instead.
//!
//! A path is a list of object keys; on an array, a number picks an item.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use super::jsonparse::{Node, Parser, blank_as_object, parse};

/// The whole file as a value. A blank file reads as an empty object.
pub fn read(text: &str) -> Result<Value> {
    parse(text)?.value(text)
}

/// The value at `path`, if there is one.
pub fn get(text: &str, path: &[&str]) -> Result<Option<Value>> {
    let root = parse(text)?;
    root.find(path).map(|n| n.value(text)).transpose()
}

/// Adds `key: value` to the object at `path`, making the objects on the way when missing.
pub fn insert(text: &str, path: &[&str], key: &str, value: &Value) -> Result<String> {
    let text = blank_as_object(text);
    let root = parse(&text)?;
    match root.find(path) {
        Some(obj @ Node::Object { members, .. }) => {
            if members.iter().any(|m| m.key == key) {
                bail!("{key} is already there");
            }
            Ok(add_member(&text, obj, key, value))
        }
        Some(_) => bail!("{} isn't an object", path.join(".")),
        None => {
            let mut nested = Map::new();
            nested.insert(key.to_string(), value.clone());
            make_path(&text, &root, path, Value::Object(nested))
        }
    }
}

/// Appends `item` to the array at `path`, making the array and the objects on the way.
pub fn push(text: &str, path: &[&str], item: &Value) -> Result<String> {
    let text = blank_as_object(text);
    let root = parse(&text)?;
    match root.find(path) {
        Some(arr @ Node::Array { .. }) => Ok(add_item(&text, arr, item)),
        Some(_) => bail!("{} isn't a list", path.join(".")),
        None => make_path(&text, &root, path, Value::Array(vec![item.clone()])),
    }
}

/// Removes `key` from the object at `path`; the text is unchanged when it isn't there.
pub fn remove(text: &str, path: &[&str], key: &str) -> Result<String> {
    let root = parse(text)?;
    let Some(obj @ Node::Object { members, .. }) = root.find(path) else {
        return Ok(text.to_string());
    };
    match members.iter().position(|m| m.key == key) {
        Some(i) => Ok(cut(text, obj, i)),
        None => Ok(text.to_string()),
    }
}

/// Removes every item of the array at `path` that `pick` chooses.
pub fn remove_items(text: &str, path: &[&str], pick: impl Fn(&Value) -> bool) -> Result<String> {
    let mut text = text.to_string();
    loop {
        let root = parse(&text)?;
        let Some(arr @ Node::Array { items, .. }) = root.find(path) else {
            return Ok(text);
        };
        let mut found = None;
        for (i, item) in items.iter().enumerate() {
            if pick(&item.value(&text)?) {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(i) => text = cut(&text, arr, i),
            None => return Ok(text),
        }
    }
}

/// Puts `leaf` at `path`, adding the missing part of the path to the deepest object found.
fn make_path(text: &str, root: &Node, path: &[&str], leaf: Value) -> Result<String> {
    let have = (0..=path.len())
        .rev()
        .find(|&n| root.find(&path[..n]).is_some())
        .unwrap_or(0);
    let parent = root.find(&path[..have]).context("no root")?;
    if !matches!(parent, Node::Object { .. }) {
        bail!("{} isn't an object", path[..have].join("."));
    }
    let mut value = leaf;
    for step in path[have + 1..].iter().rev() {
        let mut map = Map::new();
        map.insert((*step).to_string(), value);
        value = Value::Object(map);
    }
    Ok(add_member(text, parent, path[have], &value))
}

fn add_member(text: &str, obj: &Node, key: &str, value: &Value) -> String {
    let key = serde_json::to_string(key).unwrap_or_default();
    add_entry(text, obj, |indent, unit| {
        format!("{key}: {}", render(value, indent, unit))
    })
}

fn add_item(text: &str, arr: &Node, item: &Value) -> String {
    add_entry(text, arr, |indent, unit| render(item, indent, unit))
}

/// Adds an entry after the last one, laid out like its neighbours: on its own line at their
/// indent, or on the same line when the container is written on one line.
fn add_entry(text: &str, node: &Node, entry: impl Fn(&str, Option<&str>) -> String) -> String {
    let unit = indent_unit(text);
    let entries = node.entries();
    let (start, end) = (node.start(), node.end());
    let Some(&(last_start, last_end)) = entries.last() else {
        // Empty: open it up over lines, one step in from the line it is on.
        let outer = line_indent(text, start);
        let inner = format!("{outer}{unit}");
        let body = entry(&inner, Some(unit.as_str()));
        let open = start + 1;
        let close = end - 1;
        return if text[open..close].trim().is_empty() {
            format!(
                "{}\n{inner}{body}\n{outer}{}",
                &text[..open],
                &text[close..]
            )
        } else {
            // Only comments inside: keep them after the new entry.
            format!("{}\n{inner}{body}{}", &text[..open], &text[open..])
        };
    };
    let (at, comma) = after_entry(text, last_end);
    if text[start..last_start].contains('\n') {
        let indent = line_indent(text, last_start);
        let body = entry(&indent, Some(unit.as_str()));
        format!("{}{comma}\n{indent}{body}{}", &text[..at], &text[at..])
    } else {
        let body = entry("", None);
        format!("{}{comma} {body}{}", &text[..at], &text[at..])
    }
}

/// Where to add after an entry ending at `end`: past a trailing comma when there is one
/// (JSON with comments allows it), else right after the entry with a new comma.
fn after_entry(text: &str, end: usize) -> (usize, &'static str) {
    let mut p = Parser::new(text);
    p.pos = end;
    if p.skip_space().is_ok() && p.peek() == Some(b',') {
        (p.pos + 1, "")
    } else {
        (end, ",")
    }
}

/// Removes entry `i` and the comma that went with it.
fn cut(text: &str, node: &Node, i: usize) -> String {
    let entries = node.entries();
    let (start, end) = entries[i];
    if entries.len() == 1 {
        return format!("{}{}", &text[..node.start() + 1], &text[node.end() - 1..]);
    }
    if let Some(&(next, _)) = entries.get(i + 1) {
        format!("{}{}", &text[..start], &text[next..])
    } else {
        let (_, prev_end) = entries[i - 1];
        format!("{}{}", &text[..prev_end], &text[end..])
    }
}

/// Keys of what connect writes, in the order people write them (serde_json here sorts keys
/// A to Z); any others follow A to Z.
const KEY_ORDER: [&str; 7] = [
    "version", "type", "matcher", "command", "args", "async", "hooks",
];

/// `value` as JSON, pretty over lines at `indent` when `unit` is given, else on one line.
fn render(value: &Value, indent: &str, unit: Option<&str>) -> String {
    let mut out = String::new();
    write_value(&mut out, value, indent, unit);
    out
}

fn write_value(out: &mut String, value: &Value, indent: &str, unit: Option<&str>) {
    let (open, close, entries): (char, char, Vec<(Option<&String>, &Value)>) = match value {
        Value::Object(map) if !map.is_empty() => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by_key(|k| {
                let rank = KEY_ORDER.iter().position(|o| o == k);
                (rank.unwrap_or(KEY_ORDER.len()), k.as_str())
            });
            let entries = keys.into_iter().map(|k| (Some(k), &map[k])).collect();
            ('{', '}', entries)
        }
        Value::Array(items) if !items.is_empty() => {
            ('[', ']', items.iter().map(|v| (None, v)).collect())
        }
        // Scalars, `{}` and `[]`.
        other => {
            out.push_str(&serde_json::to_string(other).unwrap_or_default());
            return;
        }
    };
    let inner = format!("{indent}{}", unit.unwrap_or(""));
    out.push(open);
    for (i, (key, v)) in entries.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match unit {
            Some(_) => {
                out.push('\n');
                out.push_str(&inner);
            }
            None if i > 0 => out.push(' '),
            None => {}
        }
        if let Some(key) = key {
            out.push_str(&serde_json::to_string(key).unwrap_or_default());
            out.push_str(": ");
        }
        write_value(out, v, &inner, unit);
    }
    if unit.is_some() {
        out.push('\n');
        out.push_str(indent);
    }
    out.push(close);
}

/// The spaces or tabs at the start of the line `pos` is on.
fn line_indent(text: &str, pos: usize) -> String {
    let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    text[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// The file's own indent step: the first indented line's, else two spaces.
fn indent_unit(text: &str) -> String {
    text.lines()
        .map(|l| {
            l.chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect::<String>()
        })
        .find(|i| !i.is_empty())
        .unwrap_or_else(|| "  ".to_string())
}

#[cfg(test)]
#[path = "jsonedit_tests.rs"]
mod tests;
