//! Installed games, from the stores' own records: Steam's library folders and app
//! manifests, Epic's launcher manifests (Windows) and Xbox app folders (Windows).

use std::path::{Path, PathBuf};

use pocketgpu_proto::msg::control::Store;

#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub store: Store,
    /// Where it's installed, normalised by [`norm`] for matching.
    pub dir: String,
}

/// Every game the stores on this machine know about.
pub fn installed() -> Vec<Installed> {
    #[cfg(windows)]
    return [steam(), epic(), xbox()].concat();
    #[cfg(not(windows))]
    steam()
}

/// A path for comparing: lowercase, forward slashes, no drive letter. Proton games show
/// Linux paths as `Z:\home\…`, which this turns back into `/home/…`.
pub fn norm(path: &str) -> String {
    let p = path.replace('\\', "/").to_lowercase();
    let p = match p.as_bytes() {
        [d, b':', ..] if d.is_ascii_alphabetic() => p[2..].to_string(),
        _ => p,
    };
    p.trim_end_matches('/').to_string()
}

fn steam() -> Vec<Installed> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for root in steam_roots() {
        let Ok(folders) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) else {
            continue;
        };
        let mut libraries: Vec<PathBuf> = vdf_values(&folders, "path").map(PathBuf::from).collect();
        libraries.push(root.clone());
        for lib in libraries {
            let apps = lib.join("steamapps");
            let key = norm(&apps.to_string_lossy());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            out.extend(steam_library(&apps));
        }
    }
    out
}

fn steam_library(apps: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(apps) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("appmanifest_") && n.ends_with(".acf")
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| {
            let id = vdf_values(&text, "appid").next()?;
            let name = vdf_values(&text, "name").next()?;
            let folder = vdf_values(&text, "installdir").next()?;
            // Proton, the Steam runtime and redistributables are tools, not games.
            if is_tool(&name) {
                return None;
            }
            let dir = apps.join("common").join(folder);
            Some(Installed {
                id,
                name,
                store: Store::Steam,
                dir: norm(&dir.to_string_lossy()),
            })
        })
        .collect()
}

fn is_tool(name: &str) -> bool {
    let n = name.to_lowercase();
    [
        "proton",
        "steam linux runtime",
        "steamworks",
        "redistributable",
    ]
    .iter()
    .any(|t| n.contains(t))
}

fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
        for rel in [
            ".steam/steam",
            ".local/share/Steam",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            "snap/steam/common/.local/share/Steam",
            "Library/Application Support/Steam",
        ] {
            roots.push(home.join(rel));
        }
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(p) = std::env::var_os(var) {
            roots.push(PathBuf::from(p).join("Steam"));
        }
    }
    roots.retain(|r| r.join("steamapps").is_dir());
    roots
}

/// The values of `"key" "value"` pairs anywhere in a Valve KeyValues (VDF/ACF) file.
fn vdf_values<'a>(text: &'a str, key: &'a str) -> impl Iterator<Item = String> + 'a {
    text.lines().filter_map(move |line| {
        let mut parts = line.trim().split('"').filter(|s| !s.trim().is_empty());
        let k = parts.next()?;
        let v = parts.next()?;
        k.eq_ignore_ascii_case(key).then(|| v.replace("\\\\", "\\"))
    })
}

#[cfg(windows)]
fn epic() -> Vec<Installed> {
    let Some(data) = std::env::var_os("ProgramData") else {
        return Vec::new();
    };
    let dir = PathBuf::from(data).join("Epic/EpicGamesLauncher/Data/Manifests");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "item"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| epic_item(&text))
        .collect()
}

#[cfg_attr(not(windows), allow(dead_code))]
fn epic_item(text: &str) -> Option<Installed> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(Installed {
        id: v.get("AppName")?.as_str()?.to_string(),
        name: v.get("DisplayName")?.as_str()?.to_string(),
        store: Store::Epic,
        dir: norm(v.get("InstallLocation")?.as_str()?),
    })
}

/// Xbox app (Game Pass) installs: `X:\XboxGames\<Game>` on any drive.
#[cfg(windows)]
fn xbox() -> Vec<Installed> {
    let mut out = Vec::new();
    for drive in b'C'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\XboxGames", drive as char));
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in entries.flatten().filter(|e| e.path().is_dir()) {
            let name = e.file_name().to_string_lossy().to_string();
            if name.eq_ignore_ascii_case("GameSave") {
                continue;
            }
            out.push(Installed {
                id: name.clone(),
                name,
                store: Store::Xbox,
                dir: norm(&e.path().to_string_lossy()),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_compare_across_platforms() {
        assert_eq!(
            norm("D:\\SteamLibrary\\steamapps\\common\\Game\\"),
            "/steamlibrary/steamapps/common/game"
        );
        assert_eq!(
            norm("Z:\\home\\gan\\.steam\\steam\\steamapps\\common\\Game"),
            "/home/gan/.steam/steam/steamapps/common/game"
        );
        assert_eq!(norm("/home/gan/Games"), "/home/gan/games");
    }

    #[test]
    fn reads_steam_manifests() {
        let folders =
            "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}";
        assert_eq!(
            vdf_values(folders, "path").collect::<Vec<_>>(),
            ["D:\\SteamLibrary"]
        );
        let dir = std::env::temp_dir().join(format!("pgpu-steam-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("appmanifest_1091500.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"1091500\"\n\t\"name\"\t\t\"Neon Drift\"\n\t\"installdir\"\t\t\"Neon Drift\"\n}",
        )
        .unwrap();
        std::fs::write(
            dir.join("appmanifest_1493710.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"1493710\"\n\t\"name\"\t\t\"Proton Experimental\"\n\t\"installdir\"\t\t\"Proton - Experimental\"\n}",
        )
        .unwrap();
        let games = steam_library(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].id, "1091500");
        assert_eq!(games[0].name, "Neon Drift");
        assert!(games[0].dir.ends_with("/common/neon drift"));
    }

    #[test]
    fn reads_epic_manifests() {
        let g = epic_item(r#"{"AppName":"Fiesta","DisplayName":"Star Haul","InstallLocation":"C:\\Program Files\\Epic Games\\StarHaul"}"#).unwrap();
        assert_eq!(
            (g.id.as_str(), g.name.as_str(), g.dir.as_str()),
            ("Fiesta", "Star Haul", "/program files/epic games/starhaul")
        );
        assert!(epic_item("{}").is_none());
    }
}
