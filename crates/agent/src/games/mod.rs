//! Now playing (5.1): which installed games are running, matched by where their processes
//! were started from, with the GPU and video memory they use. The game with the most video
//! memory (else memory) is the main one. Closing a game is the GPU process kill (F09).

mod library;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use pocketgpu_proto::msg::control::{Game, Store};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::daemon::state::Agent;
use library::{Installed, norm};

/// Install records change rarely; reading them is a directory walk.
const LIBRARY_TTL: Duration = Duration::from_secs(5 * 60);

/// Helpers that live in a game's folder but aren't the game.
const HELPERS: &[&str] = &[
    "crash",
    "anticheat",
    "redist",
    "setup",
    "unins",
    "launcherhelper",
];

/// Running games, the main one first.
pub async fn running(agent: &Arc<Agent>) -> Vec<Game> {
    if std::env::var("PGPU_FAKE_GAMES").is_ok_and(|v| v != "0") {
        return fake();
    }
    let gpu: HashMap<u32, (u32, Option<u64>)> = agent
        .sampler
        .gpus()
        .processes()
        .into_iter()
        .map(|p| (p.pid, (p.gpu, p.vram_mb)))
        .collect();
    tokio::task::spawn_blocking(move || find(&library(), &gpu))
        .await
        .unwrap_or_default()
}

fn library() -> Vec<Installed> {
    type Cached = Option<(Instant, Vec<Installed>)>;
    static CACHE: OnceLock<Mutex<Cached>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut slot = cache.lock().expect("games cache poisoned");
    if let Some((at, list)) = slot.as_ref()
        && at.elapsed() < LIBRARY_TTL
    {
        return list.clone();
    }
    let list = library::installed();
    *slot = Some((Instant::now(), list.clone()));
    list
}

/// One process, as far as matching needs it.
#[derive(Debug, Clone)]
struct Proc {
    pid: u32,
    /// The executable, working directory and command line, normalised.
    places: Vec<String>,
    exe_name: String,
    memory: u64,
    started_s: u64,
}

fn find(installed: &[Installed], gpu: &HashMap<u32, (u32, Option<u64>)>) -> Vec<Game> {
    if installed.is_empty() {
        return Vec::new();
    }
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_memory(),
    );
    let procs: Vec<Proc> = sys
        .processes()
        .values()
        .map(|p| Proc {
            pid: p.pid().as_u32(),
            places: [
                p.exe().map(|e| e.to_string_lossy().to_string()),
                p.cwd().map(|c| c.to_string_lossy().to_string()),
                Some(
                    p.cmd()
                        .iter()
                        .map(|a| a.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            ]
            .into_iter()
            .flatten()
            .map(|s| norm(&s))
            .collect(),
            exe_name: p.name().to_string_lossy().to_lowercase(),
            memory: p.memory(),
            started_s: p.start_time(),
        })
        .collect();
    group(installed, &procs, gpu)
}

/// The games among `procs`, one entry per game, the main one first.
fn group(
    installed: &[Installed],
    procs: &[Proc],
    gpu: &HashMap<u32, (u32, Option<u64>)>,
) -> Vec<Game> {
    let mut by_game: HashMap<&str, Vec<&Proc>> = HashMap::new();
    for p in procs {
        if HELPERS.iter().any(|h| p.exe_name.contains(h)) {
            continue;
        }
        let game = installed.iter().find(|g| {
            let inside = format!("{}/", g.dir);
            p.places.iter().any(|s| s.contains(&inside))
        });
        if let Some(g) = game {
            by_game.entry(g.id.as_str()).or_default().push(p);
        }
    }
    let mut games: Vec<(Game, u64)> = by_game
        .into_iter()
        .filter_map(|(id, ps)| {
            let g = installed.iter().find(|g| g.id == id)?;
            let vram: u64 = ps.iter().filter_map(|p| gpu.get(&p.pid)?.1).sum();
            let on_gpu = ps.iter().find_map(|p| gpu.get(&p.pid).map(|x| x.0));
            let main = ps
                .iter()
                .max_by_key(|p| (gpu.get(&p.pid).and_then(|x| x.1).unwrap_or(0), p.memory))?;
            let started = ps.iter().map(|p| p.started_s).min().unwrap_or(0);
            let memory = ps.iter().map(|p| p.memory).sum();
            Some((
                Game {
                    id: g.id.clone(),
                    name: g.name.clone(),
                    store: g.store,
                    pid: main.pid,
                    started_ms: (started as i64) * 1000,
                    gpu: on_gpu,
                    vram_mb: on_gpu.map(|_| vram),
                },
                memory,
            ))
        })
        .collect();
    games.sort_by(|(a, am), (b, bm)| {
        b.vram_mb
            .unwrap_or(0)
            .cmp(&a.vram_mb.unwrap_or(0))
            .then(bm.cmp(am))
            .then(b.started_ms.cmp(&a.started_ms))
    });
    games.into_iter().map(|(g, _)| g).collect()
}

/// `PGPU_FAKE_GAMES=1`, for trying the app without a game running.
fn fake() -> Vec<Game> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    vec![
        Game {
            id: "1091500".into(),
            name: "Cyberpunk 2077".into(),
            store: Store::Steam,
            pid: std::process::id(),
            started_ms: now - 72 * 60_000,
            gpu: Some(0),
            vram_mb: Some(9_800),
        },
        Game {
            id: "Fiesta".into(),
            name: "Star Haul".into(),
            store: Store::Epic,
            pid: std::process::id(),
            started_ms: now - 160 * 60_000,
            gpu: Some(0),
            vram_mb: Some(6_100),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(id: &str, name: &str, dir: &str) -> Installed {
        Installed {
            id: id.into(),
            name: name.into(),
            store: Store::Steam,
            dir: norm(dir),
        }
    }

    fn proc(pid: u32, exe: &str, memory: u64, started_s: u64) -> Proc {
        Proc {
            pid,
            places: vec![norm(exe)],
            exe_name: exe.rsplit(['/', '\\']).next().unwrap_or(exe).to_lowercase(),
            memory,
            started_s,
        }
    }

    #[test]
    fn groups_processes_into_games_main_first() {
        let lib = [
            installed(
                "1",
                "Neon Drift",
                "D:\\Steam\\steamapps\\common\\Neon Drift",
            ),
            installed("2", "Star Haul", "D:\\Steam\\steamapps\\common\\Star Haul"),
        ];
        let procs = [
            proc(
                10,
                "D:\\Steam\\steamapps\\common\\Star Haul\\StarHaul.exe",
                3_000,
                100,
            ),
            proc(
                20,
                "D:\\Steam\\steamapps\\common\\Neon Drift\\bin\\NeonDrift.exe",
                8_000,
                200,
            ),
            proc(
                21,
                "D:\\Steam\\steamapps\\common\\Neon Drift\\bin\\NeonDrift-helper.exe",
                100,
                199,
            ),
            proc(
                22,
                "D:\\Steam\\steamapps\\common\\Neon Drift\\UnityCrashHandler64.exe",
                50,
                198,
            ),
            proc(30, "C:\\Program Files (x86)\\Steam\\steam.exe", 500, 50),
        ];
        let gpu = HashMap::from([(20, (0, Some(9_800))), (10, (0, Some(6_100)))]);
        let games = group(&lib, &procs, &gpu);
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].name, "Neon Drift");
        assert_eq!(games[0].pid, 20);
        assert_eq!(games[0].started_ms, 199_000);
        assert_eq!(games[0].vram_mb, Some(9_800));
        assert_eq!(games[1].name, "Star Haul");
    }

    #[test]
    fn proton_paths_match_linux_installs() {
        let lib = [installed(
            "1",
            "Neon Drift",
            "/home/gan/.steam/steam/steamapps/common/Neon Drift",
        )];
        let procs = [proc(
            5,
            "Z:\\home\\gan\\.steam\\steam\\steamapps\\common\\Neon Drift\\Game.exe",
            10,
            1,
        )];
        assert_eq!(group(&lib, &procs, &HashMap::new()).len(), 1);
    }

    #[test]
    fn a_folder_with_a_longer_name_is_not_the_game() {
        let lib = [installed("1", "Game", "/games/Game")];
        let procs = [proc(5, "/games/Game 2/game2", 10, 1)];
        assert!(group(&lib, &procs, &HashMap::new()).is_empty());
    }
}
