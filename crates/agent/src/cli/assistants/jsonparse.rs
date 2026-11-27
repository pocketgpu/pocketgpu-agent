//! A JSON reader that keeps where each value is in the text, for [`super::jsonedit`].
//! Comments and trailing commas are allowed, as many editors' settings files have them.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

/// One value's place in the text.
#[derive(Debug)]
pub enum Node {
    Object {
        start: usize,
        end: usize,
        members: Vec<Member>,
    },
    Array {
        start: usize,
        end: usize,
        items: Vec<Node>,
    },
    Scalar {
        start: usize,
        end: usize,
    },
}

#[derive(Debug)]
pub struct Member {
    pub key: String,
    /// Where the key's opening quote is.
    pub start: usize,
    pub value: Node,
}

impl Node {
    pub fn start(&self) -> usize {
        match self {
            Node::Object { start, .. } | Node::Array { start, .. } | Node::Scalar { start, .. } => {
                *start
            }
        }
    }

    pub fn end(&self) -> usize {
        match self {
            Node::Object { end, .. } | Node::Array { end, .. } | Node::Scalar { end, .. } => *end,
        }
    }

    /// Each entry's start and end: members from their key, items from their value.
    pub fn entries(&self) -> Vec<(usize, usize)> {
        match self {
            Node::Object { members, .. } => {
                members.iter().map(|m| (m.start, m.value.end())).collect()
            }
            Node::Array { items, .. } => items.iter().map(|i| (i.start(), i.end())).collect(),
            Node::Scalar { .. } => Vec::new(),
        }
    }

    fn child(&self, step: &str) -> Option<&Node> {
        match self {
            Node::Object { members, .. } => {
                members.iter().find(|m| m.key == step).map(|m| &m.value)
            }
            Node::Array { items, .. } => items.get(step.parse::<usize>().ok()?),
            Node::Scalar { .. } => None,
        }
    }

    pub fn find(&self, path: &[&str]) -> Option<&Node> {
        path.iter().try_fold(self, |node, step| node.child(step))
    }

    pub fn value(&self, text: &str) -> Result<Value> {
        Ok(match self {
            Node::Object { members, .. } => {
                let mut map = Map::new();
                for m in members {
                    map.insert(m.key.clone(), m.value.value(text)?);
                }
                Value::Object(map)
            }
            Node::Array { items, .. } => {
                Value::Array(items.iter().map(|i| i.value(text)).collect::<Result<_>>()?)
            }
            Node::Scalar { start, end } => serde_json::from_str(&text[*start..*end])?,
        })
    }
}

pub fn blank_as_object(text: &str) -> String {
    if text.trim().is_empty() {
        "{}\n".to_string()
    } else {
        text.to_string()
    }
}

pub fn parse(text: &str) -> Result<Node> {
    let text_owned;
    let text = if text.trim().is_empty() {
        text_owned = blank_as_object(text);
        &text_owned
    } else {
        text
    };
    let mut p = Parser::new(text);
    let root = p.value(0)?;
    p.skip_space()?;
    if p.pos != text.len() {
        bail!("unexpected text after the JSON at byte {}", p.pos);
    }
    Ok(root)
}

/// Reads JSON, keeping positions.
pub struct Parser<'a> {
    text: &'a str,
    pub pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    pub fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    pub fn skip_space(&mut self) -> Result<()> {
        let b = self.text.as_bytes();
        loop {
            match (self.peek(), b.get(self.pos + 1)) {
                (Some(b' ' | b'\t' | b'\n' | b'\r'), _) => self.pos += 1,
                (Some(b'/'), Some(b'/')) => {
                    self.pos = self.text[self.pos..]
                        .find('\n')
                        .map_or(self.text.len(), |i| self.pos + i);
                }
                (Some(b'/'), Some(b'*')) => {
                    let close = self.text[self.pos + 2..]
                        .find("*/")
                        .context("a comment isn't closed")?;
                    self.pos += 2 + close + 2;
                }
                _ => return Ok(()),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node> {
        if depth > 128 {
            bail!("the JSON is nested too deeply");
        }
        self.skip_space()?;
        let start = self.pos;
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => {
                self.string()?;
                Ok(Node::Scalar {
                    start,
                    end: self.pos,
                })
            }
            Some(_) => {
                let len = self.text[start..]
                    .find(|c: char| ",]}/ \t\r\n".contains(c))
                    .unwrap_or(self.text.len() - start);
                self.pos += len;
                serde_json::from_str::<Value>(&self.text[start..self.pos])
                    .with_context(|| format!("not JSON at byte {start}"))?;
                Ok(Node::Scalar {
                    start,
                    end: self.pos,
                })
            }
            None => bail!("the JSON ends too early"),
        }
    }

    fn string(&mut self) -> Result<()> {
        let b = self.text.as_bytes();
        self.pos += 1;
        loop {
            match b.get(self.pos) {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b'\\') => self.pos += 2,
                Some(_) => self.pos += 1,
                None => bail!("a string isn't closed"),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Node> {
        let start = self.pos;
        self.pos += 1;
        let mut members = Vec::new();
        loop {
            self.skip_space()?;
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Node::Object {
                        start,
                        end: self.pos,
                        members,
                    });
                }
                Some(b'"') => {
                    let key_start = self.pos;
                    self.string()?;
                    let key: String = serde_json::from_str(&self.text[key_start..self.pos])?;
                    self.skip_space()?;
                    if self.peek() != Some(b':') {
                        bail!("expected : at byte {}", self.pos);
                    }
                    self.pos += 1;
                    let value = self.value(depth + 1)?;
                    members.push(Member {
                        key,
                        start: key_start,
                        value,
                    });
                    self.after_entry(b'}')?;
                }
                _ => bail!("expected a key at byte {}", self.pos),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Node> {
        let start = self.pos;
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_space()?;
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Node::Array {
                    start,
                    end: self.pos,
                    items,
                });
            }
            items.push(self.value(depth + 1)?);
            self.after_entry(b']')?;
        }
    }

    /// After an entry: a comma, or the closing bracket (left for the caller).
    fn after_entry(&mut self, close: u8) -> Result<()> {
        self.skip_space()?;
        match self.peek() {
            Some(b',') => {
                self.pos += 1;
                Ok(())
            }
            Some(c) if c == close => Ok(()),
            _ => bail!("expected , or {} at byte {}", close as char, self.pos),
        }
    }
}
