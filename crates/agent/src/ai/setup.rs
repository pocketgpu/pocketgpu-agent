//! Set up Ollama from a phone (board 87.1, 87.2): Ollama's own build for this computer, set up
//! as the user with no admin password, then started. Linux unpacks the server into PocketGPU's
//! folder and the agent runs it, and starts it again with the agent; a Mac gets Ollama.app in
//! ~/Applications; Windows runs Ollama's own installer, which installs for this user. Shell
//! only, audited, and the computer shows a notice (notices/). The phone reads the progress kept
//! here (`tunnel::ToAgent::SetUpOllama`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pocketgpu_proto::ai::{OllamaSetup, SetupPhase};

use crate::audit::{self, Event};
use crate::daemon::state::Agent;

const BASE: &str = "https://ollama.com/download";

/// The setup since the agent started.
#[derive(Default)]
pub struct State(Mutex<OllamaSetup>);

impl State {
    pub fn get(&self) -> OllamaSetup {
        self.0.lock().expect("setup lock poisoned").clone()
    }

    fn set(&self, f: impl FnOnce(&mut OllamaSetup)) {
        f(&mut self.0.lock().expect("setup lock poisoned"));
    }

    /// Marks a setup begun unless one is under way, in one step so two phones can't both begin.
    fn begin(&self) -> bool {
        let mut s = self.0.lock().expect("setup lock poisoned");
        if matches!(
            s.phase,
            SetupPhase::Download | SetupPhase::Unpack | SetupPhase::Start
        ) {
            return false;
        }
        *s = OllamaSetup {
            phase: SetupPhase::Download,
            ..Default::default()
        };
        true
    }
}

/// One of Ollama's builds, by how it's set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Build {
    /// The Linux server, unpacked into PocketGPU's folder.
    TarZst(String),
    Tgz(String),
    /// Ollama.app, unzipped into ~/Applications.
    MacApp(String),
    /// Ollama's own installer, which installs for this user.
    WindowsSetup(String),
}

impl Build {
    fn url(&self) -> &str {
        match self {
            Self::TarZst(u) | Self::Tgz(u) | Self::MacApp(u) | Self::WindowsSetup(u) => u,
        }
    }
}

/// Ollama's builds for this system, the newest kind first (as Ollama's own install script).
pub fn builds(os: &str, arch: &str) -> Result<Vec<Build>, String> {
    match os {
        "linux" => {
            let a = match arch {
                "x86_64" => "amd64",
                "aarch64" => "arm64",
                other => return Err(format!("Ollama has no build for {other} processors")),
            };
            Ok(vec![
                Build::TarZst(format!("{BASE}/ollama-linux-{a}.tar.zst")),
                Build::Tgz(format!("{BASE}/ollama-linux-{a}.tgz")),
            ])
        }
        "macos" => Ok(vec![Build::MacApp(format!("{BASE}/Ollama-darwin.zip"))]),
        "windows" => Ok(vec![Build::WindowsSetup(format!("{BASE}/OllamaSetup.exe"))]),
        other => Err(format!("Ollama doesn't run on {other}")),
    }
}

/// Starts setting up unless it's under way; what it says now either way.
pub fn start(agent: &Arc<Agent>, device: &str) -> OllamaSetup {
    if !agent.ollama_setup.begin() {
        return agent.ollama_setup.get();
    }
    record(agent, device, "started");
    let (task, device) = (agent.clone(), device.to_string());
    tokio::spawn(async move {
        let result = run(&task).await;
        record(
            &task,
            &device,
            match &result {
                Ok(()) => "ok",
                Err(_) => "failed",
            },
        );
        task.ollama_setup.set(|s| match result {
            Ok(()) => {
                s.phase = SetupPhase::Ready;
                s.error = None;
            }
            Err(e) => {
                tracing::warn!("setting up Ollama: {e}");
                s.phase = SetupPhase::Failed;
                s.error = Some(e);
            }
        });
    });
    agent.ollama_setup.get()
}

fn record(agent: &Agent, device: &str, result: &str) {
    if let Err(e) = audit::record(&agent.db(), device, Event::OllamaSetup, "Ollama", result) {
        tracing::warn!("could not write audit entry: {e}");
    }
}

fn addr(agent: &Agent) -> String {
    agent
        .config
        .read()
        .expect("config lock poisoned")
        .ollama_addr
        .clone()
}

async fn run(agent: &Arc<Agent>) -> Result<(), String> {
    let addr = addr(agent);
    if crate::collect::ollama::version(&addr).await.is_some() {
        return Ok(());
    }
    let builds = builds(std::env::consts::OS, std::env::consts::ARCH)?;
    let home = agent.paths.data_dir.join("ollama");
    // There already but not running (say a Mac's menu bar app was quit): only started.
    let build = match builds.iter().find(|b| existing(b, &home).is_some()) {
        Some(b) => b.clone(),
        None => fetch(agent, &builds, &home).await?,
    };
    agent.ollama_setup.set(|s| s.phase = SetupPhase::Start);
    if matches!(build, Build::TarZst(_) | Build::Tgz(_)) {
        let mut cfg = agent.config.write().expect("config lock poisoned");
        if !cfg.ollama_managed {
            let mut next = cfg.clone();
            next.ollama_managed = true;
            next.save(&agent.paths.config_file())
                .map(|()| *cfg = next)
                .map_err(|e| format!("{e:#}"))?;
        }
    }
    launch(&build, &home, &addr)?;
    for _ in 0..90 {
        if crate::collect::ollama::version(&addr).await.is_some() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(format!("Ollama was set up but didn't answer at {addr}"))
}

/// Downloads the first build that exists and puts it in place.
async fn fetch(agent: &Agent, builds: &[Build], home: &Path) -> Result<Build, String> {
    let dir = agent.paths.data_dir.join("ollama-setup");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (build, file) = download(agent, builds, &dir).await?;
    agent.ollama_setup.set(|s| s.phase = SetupPhase::Unpack);
    let (b, h) = (build.clone(), home.to_path_buf());
    tokio::task::spawn_blocking(move || install(&b, &file, &h))
        .await
        .map_err(|e| e.to_string())??;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(build)
}

/// Where Ollama already is for this kind of build: PocketGPU's own Linux copy, Ollama.app in
/// either Applications folder, or the Windows app Ollama's installer puts in for the user.
fn existing(build: &Build, home: &Path) -> Option<PathBuf> {
    let found = |p: PathBuf| p.exists().then_some(p);
    match build {
        Build::TarZst(_) | Build::Tgz(_) => found(home.join("bin/ollama")),
        Build::MacApp(_) => found(PathBuf::from("/Applications/Ollama.app"))
            .or_else(|| found(home_dir().ok()?.join("Applications/Ollama.app"))),
        Build::WindowsSetup(_) => found(
            PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Programs/Ollama/ollama app.exe"),
        ),
    }
}

/// The first build that exists, downloaded into `dir`, with the progress kept for the phone.
async fn download(agent: &Agent, builds: &[Build], dir: &Path) -> Result<(Build, PathBuf), String> {
    use tokio::io::AsyncWriteExt;
    let http = reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        // A download that stalls fails, so the phone can try again.
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let mut last = String::from("no build to download");
    for build in builds {
        let mut res = match http.get(build.url()).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                last = format!("{} answered {}", build.url(), r.status());
                continue;
            }
            Err(e) => {
                last = format!("couldn't reach ollama.com: {e}");
                continue;
            }
        };
        let total = res.content_length().unwrap_or(0);
        agent.ollama_setup.set(|s| {
            s.done = 0;
            s.total = total;
        });
        let name = build.url().rsplit('/').next().unwrap_or("ollama");
        let path = dir.join(name);
        let mut out = tokio::fs::File::create(&path)
            .await
            .map_err(|e| e.to_string())?;
        let mut done = 0u64;
        while let Some(chunk) = res
            .chunk()
            .await
            .map_err(|e| format!("download stopped: {e}"))?
        {
            out.write_all(&chunk).await.map_err(|e| e.to_string())?;
            done += chunk.len() as u64;
            agent.ollama_setup.set(|s| s.done = done);
        }
        out.flush().await.map_err(|e| e.to_string())?;
        return Ok((build.clone(), path));
    }
    Err(last)
}

/// Puts the downloaded build in place.
fn install(build: &Build, file: &Path, home: &Path) -> Result<(), String> {
    let open = || std::fs::File::open(file).map_err(|e| e.to_string());
    match build {
        Build::TarZst(_) => {
            let zst = ruzstd::decoding::StreamingDecoder::new(open()?)
                .map_err(|e| format!("not a zstd file: {e}"))?;
            unpack(zst, home)
        }
        Build::Tgz(_) => unpack(flate2::read::GzDecoder::new(open()?), home),
        Build::MacApp(_) => {
            let apps = home_dir()?.join("Applications");
            std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_dir_all(apps.join("Ollama.app"));
            run_tool(
                std::process::Command::new("/usr/bin/ditto")
                    .args(["-x", "-k"])
                    .arg(file)
                    .arg(&apps),
            )
        }
        Build::WindowsSetup(_) => run_tool(std::process::Command::new(file).args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/SP-",
        ])),
    }
}

/// A tar archive into a fresh `home`.
pub fn unpack(reader: impl std::io::Read, home: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(home);
    std::fs::create_dir_all(home).map_err(|e| e.to_string())?;
    tar::Archive::new(reader)
        .unpack(home)
        .map_err(|e| format!("couldn't unpack Ollama: {e}"))
}

fn run_tool(cmd: &mut std::process::Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "setting up Ollama failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn home_dir() -> Result<PathBuf, String> {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| "no home folder".to_string())
}

/// Starts Ollama as this user.
fn launch(build: &Build, home: &Path, addr: &str) -> Result<(), String> {
    let app = existing(build, home).ok_or("Ollama wasn't where it was set up")?;
    let mut cmd = match build {
        Build::TarZst(_) | Build::Tgz(_) => server(home, addr)?,
        Build::MacApp(_) => {
            let mut c = std::process::Command::new("/usr/bin/open");
            c.arg("-a").arg(&app).args(["--args", "hidden"]);
            c
        }
        Build::WindowsSetup(_) => std::process::Command::new(&app),
    };
    cmd.stdin(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("couldn't start Ollama: {e}"))
}

/// The Linux server unpacked into `home`, listening where the agent looks for it.
fn server(home: &Path, addr: &str) -> Result<std::process::Command, String> {
    let host = addr
        .strip_prefix("http://")
        .unwrap_or(addr)
        .trim_end_matches('/');
    let log = std::fs::File::create(home.join("ollama.log")).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let mut c = std::process::Command::new(home.join("bin/ollama"));
    c.arg("serve")
        .env("OLLAMA_HOST", host)
        .stdout(log)
        .stderr(err);
    Ok(c)
}

/// With the agent: Ollama that a phone set up on Linux starts again if it isn't running.
pub fn resume(agent: Arc<Agent>) {
    let managed = agent
        .config
        .read()
        .expect("config lock poisoned")
        .ollama_managed;
    if !managed {
        return;
    }
    tokio::spawn(async move {
        let addr = addr(&agent);
        if crate::collect::ollama::version(&addr).await.is_some() {
            return;
        }
        let home = agent.paths.data_dir.join("ollama");
        if let Err(e) = server(&home, &addr).and_then(|mut c| {
            c.stdin(std::process::Stdio::null())
                .spawn()
                .map(drop)
                .map_err(|e| e.to_string())
        }) {
            tracing::warn!("couldn't start Ollama: {e}");
        }
    });
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
