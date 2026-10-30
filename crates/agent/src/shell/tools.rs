//! Which command-line tools the phone may offer to start in a terminal (5.4a). They are looked
//! up through the user's own shell, as an interactive login shell, so tools on a PATH set in
//! its startup files (npm, nvm, pipx, `~/.local/bin`) count like they do in the terminal.

use std::process::Stdio;
use std::time::Duration;

use pocketgpu_proto::shell::MAX_TOOL_NAMES;
use tokio::process::Command;

/// A slow shell startup (oh-my-zsh, nvm) still answers within this.
const TIMEOUT: Duration = Duration::from_secs(8);

/// A plain program name: nothing a shell would expand, and not an option.
pub fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}

/// The names in `names` that the user's shell finds, in the order asked. Invalid names and
/// any past [`MAX_TOOL_NAMES`] are skipped; a shell that fails or hangs finds none.
pub async fn installed(names: &[String]) -> Vec<String> {
    let names: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| valid(n))
        .take(MAX_TOOL_NAMES)
        .collect();
    if names.is_empty() {
        return Vec::new();
    }
    let mut cmd = lookup(&names);
    cmd.stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for key in crate::AGENT_ONLY_ENV {
        cmd.env_remove(key);
    }
    let out = match tokio::time::timeout(TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            tracing::debug!("tool lookup failed: {e}");
            return Vec::new();
        }
        Err(_) => {
            tracing::debug!("tool lookup timed out");
            return Vec::new();
        }
    };
    found(&names, &String::from_utf8_lossy(&out.stdout))
}

/// The names printed on a line of their own: startup files may print other things.
fn found(names: &[&str], stdout: &str) -> Vec<String> {
    names
        .iter()
        .filter(|n| stdout.lines().any(|l| l.trim() == **n))
        .map(|n| n.to_string())
        .collect()
}

#[cfg(not(windows))]
fn lookup(names: &[&str]) -> Command {
    let shell = portable_pty::CommandBuilder::new_default_prog().get_shell();
    let list = names.join(" ");
    let script = if shell.ends_with("fish") {
        format!("for t in {list}; if command -q $t; echo $t; end; end")
    } else {
        format!("for t in {list}; do command -v \"$t\" >/dev/null 2>&1 && echo \"$t\"; done")
    };
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", &script]);
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
        cmd.current_dir(home);
    }
    cmd
}

#[cfg(windows)]
fn lookup(names: &[&str]) -> Command {
    let list = names
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "foreach($t in @({list})){{ if(Get-Command $t -ErrorAction SilentlyContinue){{ $t }} }}"
    );
    let mut cmd = Command::new(crate::winsys::default_shell());
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    // No console window flashing up on the desktop.
    cmd.creation_flags(0x0800_0000);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_are_looked_up() {
        assert!(valid("claude"));
        assert!(valid("gemini-cli"));
        assert!(valid("python3.12"));
        assert!(!valid(""));
        assert!(!valid("-rf"));
        assert!(!valid("claude;rm"));
        assert!(!valid("$(id)"));
        assert!(!valid("a b"));
        assert!(!valid(&"x".repeat(65)));
    }

    #[test]
    fn startup_noise_is_ignored() {
        let out = "Welcome!\nnvm: using node 22\nclaude\n  codex  \nclaude-extra\n";
        assert_eq!(
            found(&["claude", "codex", "gemini"], out),
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn finds_programs_through_the_shell() {
        let names = vec![
            "sh".to_string(),
            "no-such-tool-pgpu".to_string(),
            "bad;name".to_string(),
        ];
        assert_eq!(installed(&names).await, vec!["sh".to_string()]);
    }
}
