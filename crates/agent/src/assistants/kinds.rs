//! What kind of action an assistant's tool call is (F32, research §5): simple rules on the
//! command text and tool name. A safety net, not a sandbox: `$(…)`, `eval`, scripts and
//! other programs stay out of reach.

use pocketgpu_proto::assistants::Kind;

/// How deep `bash -c '…'` and `find -exec` are followed.
const DEPTH_MAX: u8 = 4;

/// Tools whose input is a shell command.
pub fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool.to_ascii_lowercase().as_str(),
        "bash" | "shell" | "powershell" | "run_shell_command" | "exec_command" | "local_shell"
    )
}

/// The text a person would read: a shell tool's command (`{"command": …}` or the raw text),
/// else the input as it came.
pub fn command_text(tool: &str, input: &str) -> String {
    if is_shell_tool(tool)
        && let Ok(serde_json::Value::Object(map)) = serde_json::from_str(input)
    {
        match map.get("command") {
            Some(serde_json::Value::String(s)) => return s.clone(),
            Some(serde_json::Value::Array(argv)) => {
                return argv
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            _ => {}
        }
    }
    input.to_string()
}

/// Every switchable kind the call looks like, in `Kind::ASK_FIRST` order. Only shell tools
/// count: Write and Edit tools never do, whatever they write.
pub fn recognise(tool: &str, input: &str) -> Vec<Kind> {
    if !is_shell_tool(tool) {
        return Vec::new();
    }
    let mut found = Vec::new();
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(input)
        && let Some(serde_json::Value::Array(argv)) = map.get("command")
    {
        // Codex sends argv; its words are already split.
        let words: Vec<String> = argv
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        command(&words, 0, &mut found);
    } else {
        script(&command_text(tool, input), 0, &mut found);
    }
    Kind::ASK_FIRST
        .into_iter()
        .filter(|k| found.contains(k))
        .collect()
}

/// The kind to hold for "Ask me first", or `None` to pass: a switched-on kind, then the
/// owner's own words (case-insensitive, anywhere in the whole input).
pub fn ask_first(tool: &str, input: &str, on: &[Kind], own_words: &[String]) -> Option<Kind> {
    if let Some(k) = recognise(tool, input).into_iter().find(|k| on.contains(k)) {
        return Some(k);
    }
    let lower = input.to_lowercase();
    let text = command_text(tool, input).to_lowercase();
    own_words
        .iter()
        .map(|w| w.trim().to_lowercase())
        .any(|w| !w.is_empty() && (lower.contains(&w) || text.contains(&w)))
        .then_some(Kind::OwnWords)
}

/// What an "Ask on my phone" request is about: the first kind recognised, else `Other`.
pub fn classify(tool: &str, input: &str) -> Kind {
    recognise(tool, input)
        .into_iter()
        .next()
        .unwrap_or(Kind::Other)
}

/// A shell script: each simple command between `;`, `&&`, `||`, `|`, `&` and new lines.
fn script(text: &str, depth: u8, found: &mut Vec<Kind>) {
    if depth > DEPTH_MAX {
        return;
    }
    for words in split(text) {
        command(&words, depth, found);
    }
}

/// Words of each simple command, quotes and backslashes taken away. Operators inside
/// quotes or after a backslash are plain text; `(` and `)` split like `;`, so the inside of
/// `$(…)` and `(…)` is a command of its own.
fn split(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    let end_word = |word: &mut String, in_word: &mut bool, words: &mut Vec<String>| {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(n) = chars.next() {
                                word.push(n);
                            }
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    // A line continuation joins the lines.
                    Some('\n') | None => {}
                    Some(n) => word.push(n),
                }
            }
            ';' | '|' | '&' | '\n' | '\r' | '(' | ')' => {
                end_word(&mut word, &mut in_word, &mut words);
                if !words.is_empty() {
                    out.push(std::mem::take(&mut words));
                }
            }
            c if c.is_whitespace() => end_word(&mut word, &mut in_word, &mut words),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut word, &mut in_word, &mut words);
    if !words.is_empty() {
        out.push(words);
    }
    out
}

/// The program's name: no folder, no `.exe`, lower case.
fn base(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = name.to_lowercase();
    name.strip_suffix(".exe").map(String::from).unwrap_or(name)
}

/// Prefixes skipped before the real program, with their options that take a value.
fn prefix_options(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "sudo" => &["-u", "-g", "-h", "-p", "-r", "-t", "-C", "-D", "-U"],
        "env" => &[
            "-u",
            "-c",
            "-s",
            "-C",
            "-S",
            "--chdir",
            "--unset",
            "--split-string",
        ],
        "xargs" => &[
            "-I", "-i", "-n", "-P", "-d", "-L", "-l", "-s", "-E", "-e", "-a",
        ],
        "time" => &["-f", "-o"],
        // Shell words that start a command list, not a program.
        "nohup" | "command" | "exec" | "{" | "}" | "!" | "if" | "then" | "else" | "elif" | "do"
        | "while" | "until" => &[],
        _ => return None,
    })
}

fn command(words: &[String], depth: u8, found: &mut Vec<Kind>) {
    let mut i = 0;
    // Skip `VAR=value` and the prefixes (with their options).
    loop {
        let Some(w) = words.get(i) else { return };
        if !w.starts_with('-') && w.contains('=') && !w.starts_with('=') {
            i += 1;
            continue;
        }
        let Some(opts) = prefix_options(&base(w)) else {
            break;
        };
        i += 1;
        while let Some(o) = words.get(i) {
            if o == "--" {
                i += 1;
                break;
            }
            if !o.starts_with('-') && !(o.contains('=') && !o.starts_with('=')) {
                break;
            }
            i += if opts.contains(&o.as_str()) { 2 } else { 1 };
        }
    }
    let name = base(&words[i]);
    let args = &words[i + 1..];
    let first = first_arg(args);
    let first = first.as_deref();
    let push = |k: Kind, found: &mut Vec<Kind>| {
        if !found.contains(&k) {
            found.push(k);
        }
    };
    match name.as_str() {
        "rm" | "rmdir" | "unlink" | "shred" | "del" | "erase" | "rd" | "remove-item" | "ri" => {
            push(Kind::Delete, found)
        }
        "git" => match git_subcommand(args) {
            Some("push") => push(Kind::GitPush, found),
            Some("clean") => push(Kind::Delete, found),
            _ => {}
        },
        "find" => {
            if args.iter().any(|a| a == "-delete") {
                push(Kind::Delete, found);
            }
            let mut rest = args;
            while let Some(at) = rest
                .iter()
                .position(|a| matches!(a.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
            {
                let after = &rest[at + 1..];
                let end = after
                    .iter()
                    .position(|a| a == ";" || a == "+")
                    .unwrap_or(after.len());
                if depth < DEPTH_MAX {
                    command(&after[..end], depth + 1, found);
                }
                rest = &after[end.min(after.len())..];
            }
        }
        "kill" | "pkill" | "killall" | "taskkill" | "stop-process" | "spps" => {
            push(Kind::StopProcess, found)
        }
        "systemctl" if matches!(first, Some("stop" | "kill")) => push(Kind::StopProcess, found),
        "docker" | "podman" => {
            let sub = match first {
                Some("container") => first_arg(&args[1..]),
                other => other.map(String::from),
            };
            if matches!(sub.as_deref(), Some("stop" | "kill" | "rm")) {
                push(Kind::StopProcess, found);
            }
        }
        "pip" | "pip3" if first == Some("install") => push(Kind::Install, found),
        "uv" if first == Some("pip") && first_arg(&args[1..]).as_deref() == Some("install") => {
            push(Kind::Install, found)
        }
        "python" | "python3"
            if args.windows(3).any(|w| {
                w[0] == "-m" && matches!(w[1].as_str(), "pip" | "pip3") && w[2] == "install"
            }) =>
        {
            push(Kind::Install, found)
        }
        "npm" if matches!(first, Some("i" | "install" | "add")) => push(Kind::Install, found),
        "pnpm" | "yarn" | "bun" if first == Some("add") => push(Kind::Install, found),
        "cargo" | "brew" | "winget" if first == Some("install") => push(Kind::Install, found),
        "apt" | "apt-get" if first == Some("install") => push(Kind::Install, found),
        "bash" | "sh" | "zsh" | "dash" | "pwsh" | "powershell" => {
            if let Some(code) = shell_code(&name, args)
                && depth < DEPTH_MAX
            {
                script(code, depth + 1, found);
            }
        }
        _ => {}
    }
}

/// The first argument that isn't an option.
fn first_arg(args: &[String]) -> Option<String> {
    args.iter().find(|a| !a.starts_with('-')).cloned()
}

/// `git push` also after git's own options: `-C <dir>`, `-c <k=v>`, `--git-dir=…`.
fn git_subcommand(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" => i += 2,
            a if a.starts_with('-') => i += 1,
            a => return Some(a),
        }
    }
    None
}

/// The code of `bash -c '…'` (also `-lc`, `-ec`) or `pwsh -Command …`.
fn shell_code<'a>(name: &str, args: &'a [String]) -> Option<&'a str> {
    let pwsh = matches!(name, "pwsh" | "powershell");
    let at = args.iter().position(|a| {
        if pwsh {
            let a = a.to_lowercase();
            a == "-c" || a == "-command" || a == "/c"
        } else {
            a.starts_with('-') && !a.starts_with("--") && a.contains('c')
        }
    })?;
    if pwsh {
        let rest = &args[at + 1..];
        return rest.first().map(|s| s.as_str());
    }
    args.get(at + 1).map(|s| s.as_str())
}

#[cfg(test)]
#[path = "kinds_tests.rs"]
mod tests;
