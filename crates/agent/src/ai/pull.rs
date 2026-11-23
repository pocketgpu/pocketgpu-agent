//! Model downloads the agent runs itself (F17): Ollama's `/api/pull`, kept going while the
//! phone is in the background or the app is closed. The phone starts one, asks how far they
//! are and stops one through `tunnel::ToAgent::Pulls`. Kept in memory since the agent started;
//! Ollama keeps what it downloaded, so a download started again goes on from there.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pocketgpu_proto::pull::{ModelPull, PullMeter, model_ref};
use pocketgpu_proto::{Error, ErrorCode};
use serde_json::Value;

/// Downloads at once, so a phone can't start without end.
const MAX_RUNNING: usize = 4;
/// A finished download is forgotten after this, if no phone dismissed it.
const KEEP_DONE: Duration = Duration::from_secs(24 * 3600);

#[derive(Default)]
pub struct Pulls(Mutex<HashMap<String, Entry>>);

struct Entry {
    progress: ModelPull,
    task: Option<tokio::task::AbortHandle>,
    ended: Option<Instant>,
}

impl Pulls {
    /// The downloads, by model.
    pub fn list(&self) -> Vec<ModelPull> {
        let mut all = self.0.lock().expect("pulls lock poisoned");
        all.retain(|_, e| e.ended.is_none_or(|t| t.elapsed() < KEEP_DONE));
        let mut out: Vec<_> = all.values().map(|e| e.progress.clone()).collect();
        out.sort_by(|a, b| a.model.cmp(&b.model));
        out
    }

    /// `model` is downloading now (stopping it needs Shell; forgetting a finished one doesn't).
    pub fn running(&self, model: &str) -> bool {
        let all = self.0.lock().expect("pulls lock poisoned");
        all.get(&model_ref(model)).is_some_and(|e| !e.progress.done)
    }

    /// Stops a download, or forgets a finished one.
    pub fn stop(&self, model: &str) {
        let removed = self
            .0
            .lock()
            .expect("pulls lock poisoned")
            .remove(&model_ref(model));
        if let Some(task) = removed.and_then(|e| e.task) {
            task.abort();
        }
    }

    /// Starts downloading `model` from Ollama at `base`, unless it is under way already.
    /// `true` when it began now.
    pub fn start(
        self: &Arc<Self>,
        http: reqwest::Client,
        base: String,
        model: &str,
    ) -> Result<bool, Error> {
        let model = valid(model)?;
        let mut all = self.0.lock().expect("pulls lock poisoned");
        if all.get(&model).is_some_and(|e| !e.progress.done) {
            return Ok(false);
        }
        if all.values().filter(|e| !e.progress.done).count() >= MAX_RUNNING {
            return Err(Error::new(
                ErrorCode::Invalid,
                format!("{MAX_RUNNING} models are downloading already; wait for one to finish"),
            ));
        }
        let me = self.clone();
        let name = model.clone();
        // It waits on the lock held here until the entry is in.
        let task = tokio::spawn(async move {
            let error = run(&me, &http, &base, &name).await.err();
            me.update(&name, |p| p.finish(error));
        });
        all.insert(
            model.clone(),
            Entry {
                progress: ModelPull {
                    model,
                    status: "starting".into(),
                    ..ModelPull::default()
                },
                task: Some(task.abort_handle()),
                ended: None,
            },
        );
        Ok(true)
    }

    fn update(&self, model: &str, f: impl FnOnce(&mut ModelPull)) {
        let mut all = self.0.lock().expect("pulls lock poisoned");
        if let Some(e) = all.get_mut(model) {
            f(&mut e.progress);
            if e.progress.done && e.ended.is_none() {
                e.ended = Some(Instant::now());
                e.task = None;
            }
        }
    }
}

/// A model name such as `gemma3:12b`, with its tag.
fn valid(model: &str) -> Result<String, Error> {
    let model = model.trim();
    if model.is_empty() || model.len() > 200 || model.chars().any(char::is_whitespace) {
        return Err(Error::new(
            ErrorCode::Invalid,
            "enter a model name such as gemma3:12b",
        ));
    }
    Ok(model_ref(model))
}

/// Streams Ollama's pull and keeps its progress; `Err` says why it stopped.
async fn run(pulls: &Pulls, http: &reqwest::Client, base: &str, model: &str) -> Result<(), String> {
    let url = format!("{}/api/pull", base.trim_end_matches('/'));
    let body = serde_json::json!({ "model": model, "stream": true });
    let mut res = http.post(&url).json(&body).send().await.map_err(|e| {
        if e.is_connect() || e.is_timeout() {
            format!("Ollama isn't running on this machine (nothing answers at {base})")
        } else {
            format!("could not reach Ollama: {e}")
        }
    })?;
    let status = res.status();
    let started = Instant::now();
    let mut meter = PullMeter::default();
    let mut buf = Vec::new();
    loop {
        let chunk = res
            .chunk()
            .await
            .map_err(|e| format!("Ollama stopped answering: {e}"))?;
        let end = chunk.is_none();
        match chunk {
            Some(bytes) => buf.extend_from_slice(&bytes),
            None => buf.push(b'\n'),
        }
        while let Some(at) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=at).collect();
            if let Ok(v) = serde_json::from_slice::<Value>(&line) {
                let secs = started.elapsed().as_secs_f64();
                pulls.update(model, |p| meter.apply(p, &v, secs));
            }
        }
        if end {
            break;
        }
    }
    if !status.is_success() {
        return Err(format!("Ollama answered {}", status.as_u16()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_odd_names_and_adds_the_tag() {
        assert!(valid("").is_err());
        assert!(valid("a b").is_err());
        assert_eq!(valid(" gemma3:12b ").unwrap(), "gemma3:12b");
        assert_eq!(valid("llama3.2").unwrap(), "llama3.2:latest");
    }

    #[tokio::test]
    async fn a_download_with_no_ollama_ends_with_why() {
        let pulls = Arc::new(Pulls::default());
        let http = reqwest::Client::new();
        assert!(
            pulls
                .start(http.clone(), "http://127.0.0.1:9".into(), "gemma3")
                .unwrap()
        );
        for _ in 0..50 {
            if pulls.list()[0].done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let p = &pulls.list()[0];
        assert_eq!(p.model, "gemma3:latest");
        assert!(p.done);
        assert!(p.error.as_deref().unwrap().contains("isn't running"));
        assert!(!pulls.running("gemma3"));
        pulls.stop("gemma3");
        assert!(pulls.list().is_empty());
    }
}
