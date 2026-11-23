//! Model downloads with Ollama (F17): what the phone shows, and how Ollama's progress lines
//! become it. The agent reads them for downloads it runs (`tunnel::PullAction`); the app reads
//! them the same way for an agent too old to run them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Said when Ollama's reply ends with neither `success` nor an error.
pub const CUT_SHORT: &str = "the download stopped before it finished";

/// A model being downloaded, as the phone shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPull {
    pub model: String,
    /// Ollama's last status, such as `pulling manifest` or `success`.
    pub status: String,
    /// Bytes of the whole model so far, of `total` (the parts begun so far).
    pub completed: u64,
    pub total: u64,
    /// Bytes per second lately; 0 until known.
    pub rate: u64,
    pub error: Option<String>,
    /// Finished, stopped or failed (`error`).
    pub done: bool,
}

impl ModelPull {
    /// The download ended: with `error`, or cut short when Ollama never said `success`.
    pub fn finish(&mut self, error: Option<String>) {
        self.error = self.error.take().or(error);
        if !self.done && self.error.is_none() {
            self.error = Some(CUT_SHORT.into());
        }
        self.done = true;
    }
}

/// Reads Ollama's progress lines. Ollama counts each part of a model (its layers) from 0 and
/// counts bytes it kept from an earlier try, so the whole is the sum of the parts and the
/// speed comes from the bytes added between readings, not since the start.
#[derive(Debug, Default)]
pub struct PullMeter {
    /// Each part by its digest: bytes so far, of its size.
    parts: Vec<(String, u64, u64)>,
    /// The last reading for the speed: seconds since the start, and bytes.
    last: Option<(f64, u64)>,
    rate: f64,
}

/// How often the speed is worked out, and how much each new reading counts.
const EVERY_S: f64 = 1.0;
const WEIGHT: f64 = 0.3;

impl PullMeter {
    /// One line of Ollama's progress, `at` seconds after the download began.
    pub fn apply(&mut self, p: &mut ModelPull, v: &Value, at: f64) {
        if let Some(e) = v.get("error").and_then(Value::as_str) {
            p.error = Some(e.to_string());
        }
        let status = v.get("status").and_then(Value::as_str);
        if let Some(s) = status {
            p.status = s.to_string();
            p.done |= s == "success";
        }
        let (Some(c), Some(t)) = (
            v.get("completed").and_then(Value::as_u64),
            v.get("total").and_then(Value::as_u64),
        ) else {
            return;
        };
        let key = v
            .get("digest")
            .and_then(Value::as_str)
            .or(status)
            .unwrap_or_default();
        match self.parts.iter_mut().find(|(k, ..)| k == key) {
            Some(part) => (part.1, part.2) = (c, t),
            None => self.parts.push((key.to_string(), c, t)),
        }
        p.completed = self.parts.iter().map(|(_, c, _)| c).sum();
        p.total = self.parts.iter().map(|(.., t)| t).sum();
        match self.last {
            None => self.last = Some((at, p.completed)),
            Some((then, before)) if at - then >= EVERY_S => {
                let now = p.completed.saturating_sub(before) as f64 / (at - then);
                self.rate = if self.rate == 0.0 {
                    now
                } else {
                    self.rate + WEIGHT * (now - self.rate)
                };
                self.last = Some((at, p.completed));
                p.rate = self.rate as u64;
            }
            Some(_) => {}
        }
    }
}

/// The model's name as Ollama keeps it: `llama3.2` is `llama3.2:latest`, so both are one
/// download.
pub fn model_ref(name: &str) -> String {
    let name = name.trim();
    let last = name.rsplit('/').next().unwrap_or(name);
    if name.is_empty() || last.contains(':') {
        name.to_string()
    } else {
        format!("{name}:latest")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(digest: &str, c: u64, t: u64) -> Value {
        json!({"status": format!("pulling {digest}"), "digest": digest, "completed": c, "total": t})
    }

    #[test]
    fn adds_up_the_parts() {
        let (mut m, mut p) = (PullMeter::default(), ModelPull::default());
        m.apply(&mut p, &json!({"status": "pulling manifest"}), 0.0);
        assert_eq!(p.status, "pulling manifest");
        m.apply(&mut p, &line("a", 800, 800), 1.0);
        m.apply(&mut p, &line("b", 50, 100), 2.0);
        assert_eq!((p.completed, p.total), (850, 900));
        assert!(!p.done);
        m.apply(&mut p, &json!({"status": "success"}), 3.0);
        assert!(p.done);
    }

    #[test]
    fn speed_counts_only_new_bytes() {
        let (mut m, mut p) = (PullMeter::default(), ModelPull::default());
        // Resumed: 6 GB were there already.
        m.apply(&mut p, &line("a", 6_000_000_000, 8_000_000_000), 0.5);
        assert_eq!(p.rate, 0);
        m.apply(&mut p, &line("a", 6_010_000_000, 8_000_000_000), 1.5);
        assert_eq!(p.rate, 10_000_000);
        m.apply(&mut p, &line("a", 6_030_000_000, 8_000_000_000), 2.5);
        assert_eq!(p.rate, 13_000_000);
    }

    #[test]
    fn an_end_without_success_is_said() {
        let mut p = ModelPull::default();
        p.finish(None);
        assert_eq!(p.error.as_deref(), Some(CUT_SHORT));
        let mut p = ModelPull {
            done: true,
            ..ModelPull::default()
        };
        p.finish(None);
        assert_eq!(p.error, None);
        let mut p = ModelPull {
            error: Some("file does not exist".into()),
            ..ModelPull::default()
        };
        p.finish(Some("Ollama answered 404".into()));
        assert_eq!(p.error.as_deref(), Some("file does not exist"));
    }

    #[test]
    fn names_the_tag() {
        assert_eq!(model_ref(" llama3.2 "), "llama3.2:latest");
        assert_eq!(model_ref("gemma3:12b"), "gemma3:12b");
        assert_eq!(
            model_ref("hf.co/bartowski/Llama-3.2-1B-Instruct-GGUF"),
            "hf.co/bartowski/Llama-3.2-1B-Instruct-GGUF:latest"
        );
        assert_eq!(model_ref("hf.co/x/y:Q4_K_M"), "hf.co/x/y:Q4_K_M");
    }
}
