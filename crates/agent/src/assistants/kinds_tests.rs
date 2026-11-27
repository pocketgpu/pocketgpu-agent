use super::*;
use pocketgpu_proto::assistants::Kind::{self, *};

fn bash(cmd: &str) -> String {
    serde_json::json!({ "command": cmd, "description": "x" }).to_string()
}

/// Every form in research §5, and the ones that must not match.
#[test]
fn the_table() {
    let table: &[(&str, &[Kind])] = &[
        // delete
        ("rm -rf build dist .cache", &[Delete]),
        ("/bin/rm x", &[Delete]),
        ("rmdir out", &[Delete]),
        ("unlink a", &[Delete]),
        ("shred -u key", &[Delete]),
        ("del /s x", &[Delete]),
        ("rd /s /q x", &[Delete]),
        ("Remove-Item -Recurse x", &[Delete]),
        ("git clean -fdx", &[Delete]),
        ("find . -name '*.o' -delete", &[Delete]),
        ("find . -name x -exec rm {} \\;", &[Delete]),
        ("find . -exec /usr/bin/rm -f {} +", &[Delete]),
        // git push
        ("git push", &[GitPush]),
        ("git push --force origin main", &[GitPush]),
        ("git -C ../repo push", &[GitPush]),
        ("git -c user.name=x push", &[GitPush]),
        ("git --git-dir=.git --work-tree=. push", &[GitPush]),
        ("git --no-pager push", &[GitPush]),
        // stop processes
        ("kill -9 123", &[StopProcess]),
        ("pkill python", &[StopProcess]),
        ("killall node", &[StopProcess]),
        ("taskkill /F /PID 4", &[StopProcess]),
        ("Stop-Process -Id 4", &[StopProcess]),
        ("systemctl stop ollama", &[StopProcess]),
        ("systemctl --user kill x", &[StopProcess]),
        ("docker stop web", &[StopProcess]),
        ("docker kill web", &[StopProcess]),
        ("docker rm -f web", &[StopProcess]),
        ("docker container stop web", &[StopProcess]),
        // install
        ("pip install torch", &[Install]),
        ("pip3 install -U torch", &[Install]),
        ("uv pip install torch", &[Install]),
        ("python -m pip install x", &[Install]),
        ("npm i left-pad", &[Install]),
        ("npm install", &[Install]),
        ("pnpm add x", &[Install]),
        ("yarn add x", &[Install]),
        ("bun add x", &[Install]),
        ("cargo install ripgrep", &[Install]),
        ("brew install jq", &[Install]),
        ("apt install jq", &[Install]),
        ("apt-get install -y jq", &[Install]),
        ("winget install Git.Git", &[Install]),
        // splitting
        ("cd build && rm -rf *", &[Delete]),
        ("make; git push", &[GitPush]),
        ("false || rm x", &[Delete]),
        ("ls | xargs rm", &[Delete]),
        ("sleep 1 & kill 5", &[StopProcess]),
        ("echo hi\nrm x", &[Delete]),
        ("ls \\\n && rm x", &[Delete]),
        ("echo $(rm x)", &[Delete]),
        ("(cd a; rm b)", &[Delete]),
        ("{ rm x; }", &[Delete]),
        ("if true; then git push; fi", &[GitPush]),
        // prefixes
        ("sudo rm -rf /opt/x", &[Delete]),
        ("sudo -u root rm x", &[Delete]),
        ("env FOO=1 rm x", &[Delete]),
        ("FOO=1 BAR=2 git push", &[GitPush]),
        ("nohup kill 3", &[StopProcess]),
        ("command rm x", &[Delete]),
        ("exec rm x", &[Delete]),
        ("time rm x", &[Delete]),
        ("xargs -n 1 rm", &[Delete]),
        ("sudo env A=b nohup rm x", &[Delete]),
        // shells
        ("bash -c 'rm -rf x'", &[Delete]),
        ("sh -c \"git push\"", &[GitPush]),
        ("zsh -lc 'cd a && pip install b'", &[Install]),
        ("bash -c \"bash -c 'kill 1'\"", &[StopProcess]),
        ("pwsh -Command Remove-Item x", &[Delete]),
        // several at once
        (
            "rm a && git push && pip install b",
            &[Delete, GitPush, Install],
        ),
        // must not match
        ("ls -la", &[]),
        ("git pushd", &[]),
        ("git status", &[]),
        ("rmate file.txt", &[]),
        ("echo 'rm -rf x; git push'", &[]),
        ("grep -r 'kill' .", &[]),
        ("cat rm.txt", &[]),
        ("npm run build", &[]),
        ("pip list", &[]),
        ("docker ps", &[]),
        ("systemctl status ollama", &[]),
        ("find . -name x -print", &[]),
        ("pushd dir", &[]),
        ("", &[]),
    ];
    for (cmd, want) in table {
        assert_eq!(recognise("Bash", &bash(cmd)), want.to_vec(), "{cmd:?}");
    }
}

#[test]
fn only_shell_tools_count() {
    let write = serde_json::json!({ "file_path": "rm.sh", "content": "rm -rf /\ngit push" });
    for tool in ["Write", "Edit", "MultiEdit", "NotebookEdit", "mcp__x__run"] {
        assert!(recognise(tool, &write.to_string()).is_empty(), "{tool}");
    }
    assert_eq!(
        recognise("PowerShell", &bash("Remove-Item x")),
        vec![Delete]
    );
    // A raw command, and Codex's argv.
    assert_eq!(recognise("shell", "rm -rf x"), vec![Delete]);
    let argv = serde_json::json!({ "command": ["bash", "-lc", "git push"] });
    assert_eq!(recognise("shell", &argv.to_string()), vec![GitPush]);
    assert_eq!(
        command_text("shell", &argv.to_string()),
        "bash -lc git push"
    );
    assert_eq!(command_text("Bash", &bash("ls")), "ls");
    assert_eq!(command_text("Write", "{\"a\":1}"), "{\"a\":1}");
}

#[test]
fn ask_first_uses_the_switches_and_own_words() {
    let words = vec!["Terraform Apply".to_string()];
    assert_eq!(
        ask_first("Bash", &bash("rm x"), &[Delete], &[]),
        Some(Delete)
    );
    assert_eq!(ask_first("Bash", &bash("rm x"), &[GitPush], &[]), None);
    assert_eq!(
        ask_first(
            "Bash",
            &bash("cd infra && terraform apply -auto-approve"),
            &[],
            &words
        ),
        Some(OwnWords)
    );
    assert_eq!(
        ask_first("Bash", &bash("terraform plan"), &[], &words),
        None
    );
    // Own words look at every tool's whole input, Write included.
    let w = serde_json::json!({ "file_path": "run.sh", "content": "TERRAFORM APPLY" });
    assert_eq!(
        ask_first("Write", &w.to_string(), &[Delete], &words),
        Some(OwnWords)
    );
    assert_eq!(ask_first("Bash", &bash("ls"), &[], &[" ".into()]), None);
}

#[test]
fn classify_names_the_first_kind_or_other() {
    assert_eq!(classify("Bash", &bash("git push && rm x")), Delete);
    assert_eq!(classify("Bash", &bash("ls")), Other);
    assert_eq!(classify("Edit", "{}"), Other);
}
