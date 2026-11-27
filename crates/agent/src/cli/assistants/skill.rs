//! The skill: a short note that tells an assistant when to use PocketGPU's tools. The same
//! text ships in the Claude Code plugin (`oss/public/plugins/pocketgpu/skills/pocketgpu`); a
//! test keeps the two copies equal.

use std::path::Path;

use anyhow::Result;

use super::files;
use super::manifest::Manifest;
use super::places::Places;
use super::plan::{Action, Assistant, FileUndo, Part, Step};

pub const TEXT: &str = include_str!("SKILL.md");

/// What to do about the skill in `skills` (a skills folder): `Ok(None)` when it is there
/// already, or when a skill of the user's own has the name.
pub fn step(
    assistant: Assistant,
    skills: &Path,
    places: &Places,
    manifest: &Manifest,
) -> Result<Option<Step>> {
    let path = skills.join("pocketgpu").join("SKILL.md");
    let before = files::read(&path)?;
    match &before {
        Some(text) if text == TEXT => return Ok(None),
        // Ours from an older version: bring it up to date.
        Some(text) if manifest.wrote(&path, text) => {}
        Some(_) => return Ok(None),
        None => {}
    }
    Ok(Some(Step {
        assistant,
        part: Part::Skill,
        detail: places.show(&path),
        action: Action::Write {
            path,
            before,
            after: TEXT.to_string(),
            undo: FileUndo::Delete,
        },
    }))
}

/// Whether a PocketGPU skill is in `skills`.
pub fn present(skills: &Path) -> bool {
    skills.join("pocketgpu").join("SKILL.md").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plugin's copy: under `oss/public` here, at the root of the public repository.
    #[test]
    fn the_plugin_has_the_same_skill() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rel = "plugins/pocketgpu/skills/pocketgpu/SKILL.md";
        let copies: Vec<String> = [
            crate_dir.join("../../oss/public").join(rel),
            crate_dir.join("../..").join(rel),
        ]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
        assert!(!copies.is_empty(), "the plugin's SKILL.md is missing");
        for copy in copies {
            assert_eq!(copy, TEXT, "the plugin's SKILL.md differs from the agent's");
        }
    }

    #[test]
    fn the_skill_has_what_assistants_need() {
        // A Windows checkout may have turned the line ends into CRLF.
        let text = TEXT.replace("\r\n", "\n");
        let front = text
            .strip_prefix("---\nname: pocketgpu\ndescription: ")
            .unwrap();
        assert!(front.contains("\n---\n"));
        for tool in ["machine_status", "list_jobs", "job_output", "notify_phone"] {
            assert!(TEXT.contains(tool), "{tool}");
        }
        for command in ["pgpu run", "pgpu send <file>", "pgpu clip put"] {
            assert!(TEXT.contains(command), "{command}");
        }
        assert!(TEXT.contains("Never put secrets"));
    }
}
