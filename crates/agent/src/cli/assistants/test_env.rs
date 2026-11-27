//! A made-up home folder with all three assistants and settings of the user's own, and fake
//! `claude` and `codex` command lines that edit their files the way the real ones do.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use super::flow::{self, Opts, Report};
use super::places::{Output, Places, Runner};
use super::{Ctx, jsonedit, manifest::Manifest};

pub(super) const SETTINGS: &str = r#"{
  "model": "opus",
  "hooks": {
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "afplay /System/Library/Sounds/Glass.aiff" }
        ]
      }
    ]
  },
  "enabledPlugins": { "other@market": true }
}
"#;
pub(super) const CLAUDE_JSON: &str =
    "{\"numStartups\": 3, \"mcpServers\": {\"x\": {\"command\": \"y\"}}}";
pub(super) const CODEX_CONFIG: &str = "model = \"gpt-5\"\nnotify = [\"say\", \"done\"]\n\n\
                            [mcp_servers.other]\ncommand = \"other\"\n";
pub(super) const CODEX_HOOKS: &str = "{\n  \"hooks\": {\n    \"Stop\": [\n      { \"hooks\": [{ \
                           \"type\": \"command\", \"command\": \"mine\" }] }\n    ]\n  }\n}\n";
pub(super) const CURSOR_MCP: &str = "{\n  \"mcpServers\": {\n    \"github\": {\n      \"command\": \"gh-mcp\",\n      \
                          \"env\": { \"TOKEN\": \"t\" }\n    }\n  }\n}\n";

pub(super) struct Env {
    _dir: tempfile::TempDir,
    pub(super) places: Places,
    pub(super) exe: PathBuf,
    pub(super) fake: Fake,
}

/// `claude` and `codex` as far as connect uses them. Removing gives back the file as it was.
pub(super) struct Fake {
    places: Places,
    have: Vec<&'static str>,
    pub(super) plugins: String,
    /// What `claude --version` prints.
    pub(super) version: String,
    pub(super) calls: RefCell<Vec<String>>,
    saved: RefCell<HashMap<PathBuf, Option<String>>>,
}

impl Runner for Fake {
    fn find(&self, program: &str) -> Option<PathBuf> {
        self.have
            .contains(&program)
            .then(|| PathBuf::from(format!("/fake/{program}")))
    }

    fn run(&self, program: &Path, args: &[String]) -> Result<Output> {
        let name = program.file_name().unwrap().to_string_lossy().into_owned();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.calls
            .borrow_mut()
            .push(format!("{name} {}", args.join(" ")));
        let ok = |stdout: &str| Output {
            ok: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        };
        let file = match name.as_str() {
            "claude" => self.places.claude_json.clone(),
            _ => self.places.codex_home.join("config.toml"),
        };
        match (name.as_str(), args.as_slice()) {
            ("claude", ["plugin", "list", "--json"]) => Ok(ok(&self.plugins)),
            ("claude", ["--version"]) => Ok(ok(&self.version)),
            (
                "claude",
                [
                    "mcp",
                    "add",
                    "--scope",
                    "user",
                    "pocketgpu",
                    "--",
                    exe,
                    "mcp",
                    "--assistant",
                    "claude",
                ],
            ) => {
                let text = self.keep(&file);
                let server = json!({ "type": "stdio", "command": exe,
                    "args": ["mcp", "--assistant", "claude"] });
                let new = jsonedit::insert(&text, &["mcpServers"], "pocketgpu", &server)?;
                std::fs::write(&file, new)?;
                Ok(ok("Added"))
            }
            (
                "codex",
                [
                    "mcp",
                    "add",
                    "pocketgpu",
                    "--",
                    exe,
                    "mcp",
                    "--assistant",
                    "codex",
                ],
            ) => {
                let text = self.keep(&file);
                let table = format!(
                    "\n[mcp_servers.pocketgpu]\ncommand = {exe:?}\nargs = [\"mcp\", \"--assistant\", \"codex\"]\n"
                );
                std::fs::write(&file, format!("{text}{table}"))?;
                Ok(ok("Added"))
            }
            ("claude" | "codex", ["mcp", "remove", ..]) => {
                match self.saved.borrow_mut().remove(&file).flatten() {
                    Some(text) => std::fs::write(&file, text)?,
                    None => std::fs::remove_file(&file)?,
                }
                Ok(ok("Removed"))
            }
            _ => anyhow::bail!("unexpected: {name} {args:?}"),
        }
    }
}

impl Fake {
    fn keep(&self, file: &Path) -> String {
        let text = std::fs::read_to_string(file).ok();
        self.saved
            .borrow_mut()
            .insert(file.to_path_buf(), text.clone());
        text.unwrap_or_default()
    }
}

/// A home with all three assistants and settings of the user's own in them.
pub(super) fn env(have: &[&'static str]) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let places = Places::under(&dir.path().join("home"), &dir.path().join("data"));
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(&places.claude_dir.join("settings.json"), SETTINGS);
    write(&places.claude_json, CLAUDE_JSON);
    write(&places.codex_home.join("config.toml"), CODEX_CONFIG);
    write(&places.codex_home.join("hooks.json"), CODEX_HOOKS);
    write(&places.cursor_dir.join("mcp.json"), CURSOR_MCP);
    // A space in the path, as under "C:\Users\Jo Smith".
    let exe = dir.path().join("my bin").join("pocketgpu");
    let fake = Fake {
        places: places.clone(),
        have: have.to_vec(),
        plugins: "[]".into(),
        version: "2.1.332 (Claude Code)\n".into(),
        calls: RefCell::default(),
        saved: RefCell::default(),
    };
    Env {
        _dir: dir,
        places,
        exe,
        fake,
    }
}

impl Env {
    pub(super) fn connect(&self, opts: Opts, answer: bool) -> (Result<Report>, String, usize) {
        let manifest = Manifest::load(&self.places).unwrap();
        let ctx = self.ctx(&manifest);
        let mut asked = 0;
        let mut ask = |_: &str| {
            asked += 1;
            answer
        };
        let mut out = Vec::new();
        let report = flow::connect(&ctx, opts, &mut ask, &mut out);
        (report, String::from_utf8(out).unwrap(), asked)
    }

    pub(super) fn disconnect(&self) -> Report {
        let manifest = Manifest::load(&self.places).unwrap();
        let mut out = Vec::new();
        flow::disconnect(&self.ctx(&manifest), YES, &mut |_| true, &mut out).unwrap()
    }

    pub(super) fn ctx<'a>(&'a self, manifest: &'a Manifest) -> Ctx<'a> {
        Ctx {
            places: &self.places,
            runner: &self.fake,
            exe: &self.exe,
            manifest,
        }
    }

    pub(super) fn read(&self, path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    pub(super) fn json(&self, path: &Path) -> Value {
        jsonedit::read(&self.read(path)).unwrap()
    }
}

pub(super) const YES: Opts = Opts {
    dry_run: false,
    yes: true,
    json: false,
};

/// Every file and folder under `root`, with the files' bytes.
pub(super) fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                todo.push(path);
                out.insert(rel, None);
            } else {
                out.insert(rel, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    out
}
