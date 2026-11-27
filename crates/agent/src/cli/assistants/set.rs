//! `pgpu assistants set …` (F32): the `[assistants]` settings, changed through the running
//! agent so it uses them at once, audits the change and rewrites its guard file. The agent
//! never sees the plan, so turning a switch on always says what it needs on the phone.

use anyhow::{Result, bail};
use clap::{Subcommand, ValueEnum};
use pocketgpu_proto::Permission;
use pocketgpu_proto::assistants::{AssistantSettings, Kind, OWN_WORD_LEN, OWN_WORDS_MAX};
use pocketgpu_proto::local::{Request, Response};

use crate::ipc::client;
use crate::store::paths::Paths;

#[derive(Debug, Clone, Subcommand)]
pub enum What {
    /// When Claude Code would ask you at the computer, ask your Full control phones too
    /// (on or off). Answering on the phone needs PocketGPU Pro on the phone.
    AskOnPhone {
        #[arg(value_enum)]
        state: OnOff,
    },
    /// Kinds of action that always wait for your phone, even in auto mode: delete, git-push,
    /// stop, install (several at once), or none. No answer in 5 minutes: they don't run.
    /// Needs Claude Code 2.1.295 or newer, or Cursor.
    AskFirst {
        #[arg(value_enum, required = true, num_args = 1..)]
        kinds: Vec<AskFirstKind>,
    },
    /// Your own words: a command containing any of them waits for your phone too, such as
    /// "terraform apply" (up to 20, quoted when they have spaces). None clears them.
    Words {
        #[arg(num_args = 0..)]
        words: Vec<String>,
    },
    /// `pgpu assistants set <assistant> view|control|shell`: what that assistant may do through
    /// PocketGPU's own tools. View (the default): status, jobs and notes; Control: also offer
    /// files and stop its own jobs; Shell: also run saved actions.
    #[command(external_subcommand)]
    Level(Vec<String>),
}

/// The kinds `ask-first` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AskFirstKind {
    Delete,
    GitPush,
    Stop,
    Install,
    /// Turns them all off.
    None,
}

impl AskFirstKind {
    fn kind(self) -> Option<Kind> {
        match self {
            AskFirstKind::Delete => Some(Kind::Delete),
            AskFirstKind::GitPush => Some(Kind::GitPush),
            AskFirstKind::Stop => Some(Kind::StopProcess),
            AskFirstKind::Install => Some(Kind::Install),
            AskFirstKind::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

pub async fn run(paths: &Paths, json: bool, what: What) -> Result<()> {
    let mut settings = settings_of(client::call(paths, Request::GetAssistantSettings).await?)?;
    apply(&mut settings, what)?;
    let req = Request::SetAssistantSettings(settings);
    let settings = settings_of(client::call(paths, req).await?)?;
    if json {
        let kinds: Vec<&str> = settings
            .ask_first
            .iter()
            .filter_map(|k| k.config_name())
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "ask_on_phone": settings.ask_on_phone,
                "ask_first": kinds,
                "own_words": settings.own_words,
                "levels": settings
                    .levels
                    .iter()
                    .map(|(id, p)| (id.clone(), p.as_str()))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            })
        );
    } else {
        print!("{}", words(&settings));
    }
    Ok(())
}

/// The change asked for, checked here so a mistake says so before the agent is asked.
fn apply(settings: &mut AssistantSettings, what: What) -> Result<()> {
    match what {
        What::AskOnPhone { state } => settings.ask_on_phone = state == OnOff::On,
        What::AskFirst { kinds } => {
            if kinds.contains(&AskFirstKind::None) && kinds.len() > 1 {
                bail!("none can't go with other kinds");
            }
            // In the app's order, each once.
            settings.ask_first = Kind::ASK_FIRST
                .into_iter()
                .filter(|k| kinds.iter().any(|a| a.kind() == Some(*k)))
                .collect();
        }
        What::Words { words } => {
            let mut kept: Vec<String> = Vec::new();
            for w in words {
                let w = w.trim().to_string();
                if w.is_empty() || kept.iter().any(|k| k.eq_ignore_ascii_case(&w)) {
                    continue;
                }
                if w.chars().count() > OWN_WORD_LEN {
                    bail!("\"{w}\" is too long: at most {OWN_WORD_LEN} characters");
                }
                kept.push(w);
            }
            if kept.len() > OWN_WORDS_MAX {
                bail!("at most {OWN_WORDS_MAX} words");
            }
            settings.own_words = kept;
        }
        What::Level(args) => {
            let [id, level] = args.as_slice() else {
                bail!(
                    "use: pgpu assistants set <assistant> view|control|shell, such as \
                     pgpu assistants set claude control"
                );
            };
            let id = crate::assistants::seen::canonical(id.trim());
            if !crate::store::config::valid_assistant_id(&id) {
                bail!(
                    "unknown setting or assistant {id:?}; use ask-on-phone, ask-first, words, \
                     or an assistant such as claude, codex, cursor or gemini"
                );
            }
            let level: Permission = level
                .trim()
                .to_ascii_lowercase()
                .parse()
                .map_err(anyhow::Error::msg)?;
            settings.levels.retain(|(i, _)| *i != id);
            // Missing means View, so View needs no line.
            if level != Permission::View {
                settings.levels.push((id, level));
                settings.levels.sort();
            }
        }
    }
    Ok(())
}

fn settings_of(reply: Response) -> Result<AssistantSettings> {
    match reply {
        Response::AssistantSettings(s) => Ok(s),
        Response::Error(e) => bail!(e),
        // An agent from before approvals doesn't know the request.
        other => bail!("the agent is too old for this; update it with pgpu update ({other:?})"),
    }
}

pub fn words(s: &AssistantSettings) -> String {
    let mut text = String::from(if s.ask_on_phone {
        "Ask on my phone is on: when Claude Code would ask you at the computer, your Full \
         control phones get a push and can Allow or Deny. You can still answer at the computer.\n"
    } else {
        "Ask on my phone is off: Claude Code asks at the computer only.\n"
    });
    let kinds: Vec<&str> = s.ask_first.iter().map(|k| kind_words(*k)).collect();
    let first = !kinds.is_empty() || !s.own_words.is_empty();
    if first {
        let mut what = kinds.join(", ");
        if !s.own_words.is_empty() {
            if !what.is_empty() {
                what.push_str(", ");
            }
            what.push_str(&format!("your own words ({})", s.own_words.join(", ")));
        }
        text.push_str(&format!(
            "Ask me first is on for {what}: these wait for your phone even in auto mode. No \
             answer in 5 minutes: they don't run.\n\
             It's a safety net, not a wall: an assistant with a terminal can get around it.\n"
        ));
    } else {
        text.push_str("Ask me first is off.\n");
    }
    for (id, level) in &s.levels {
        text.push_str(&format!(
            "{} may {} through PocketGPU's tools.\n",
            crate::assistants::display_name(id),
            level_words(*level)
        ));
    }
    if s.ask_on_phone || first {
        text.push_str(
            "Answering on the phone needs PocketGPU Pro on the phone; without it, the \
             assistant asks at the computer, or the action doesn't run.\n\
             If your assistants aren't connected yet, run: pgpu assistants connect\n",
        );
    }
    text
}

fn level_words(p: Permission) -> &'static str {
    match p {
        Permission::View => "read status and jobs and send notes",
        Permission::Control => "also offer files and stop its own jobs",
        Permission::Shell => "also run saved actions",
    }
}

fn kind_words(k: Kind) -> &'static str {
    match k {
        Kind::Delete => "deleting files",
        Kind::GitPush => "git push",
        Kind::StopProcess => "stopping programs",
        Kind::Install => "installing packages",
        _ => "other actions",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_says_what_it_needs_on_the_phone() {
        let on = AssistantSettings {
            ask_on_phone: true,
            ..Default::default()
        };
        let text = words(&on);
        assert!(text.contains("Ask on my phone is on"));
        assert!(text.contains("needs PocketGPU Pro on the phone"));
        assert!(words(&AssistantSettings::default()).contains("is off"));
    }

    #[test]
    fn ask_first_and_words() {
        let mut s = AssistantSettings {
            own_words: vec!["old".into()],
            ..Default::default()
        };
        use AskFirstKind::*;
        let kinds = |k: Vec<AskFirstKind>| What::AskFirst { kinds: k };
        apply(&mut s, kinds(vec![Install, Delete, Delete])).unwrap();
        assert_eq!(s.ask_first, [Kind::Delete, Kind::Install]);
        assert_eq!(s.own_words, ["old"], "the rest stays");
        let text = words(&s);
        assert!(text.contains("Ask me first is on for deleting files, installing packages"));
        assert!(text.contains("your own words (old)") && text.contains("safety net"));
        assert!(apply(&mut s, kinds(vec![None, Stop])).is_err());
        apply(&mut s, kinds(vec![None])).unwrap();
        assert!(s.ask_first.is_empty());

        let w = |w: &[&str]| What::Words {
            words: w.iter().map(|s| s.to_string()).collect(),
        };
        apply(
            &mut s,
            w(&["terraform apply", " ", "Terraform Apply", "kubectl delete"]),
        )
        .unwrap();
        assert_eq!(s.own_words, ["terraform apply", "kubectl delete"]);
        assert!(apply(&mut s, w(&[&"x".repeat(OWN_WORD_LEN + 1)])).is_err());
        let many: Vec<String> = (0..=OWN_WORDS_MAX).map(|i| format!("w{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(apply(&mut s, w(&many)).is_err());
        apply(&mut s, w(&[])).unwrap();
        assert!(s.own_words.is_empty());
        assert!(words(&s).contains("Ask me first is off"));
    }

    #[test]
    fn levels_per_assistant() {
        let mut s = AssistantSettings::default();
        let set = |a: &[&str]| What::Level(a.iter().map(|s| s.to_string()).collect());
        apply(&mut s, set(&["codex", "shell"])).unwrap();
        apply(&mut s, set(&["claude-code", "Control"])).unwrap();
        assert_eq!(
            s.levels,
            [
                ("claude".to_string(), Permission::Control),
                ("codex".to_string(), Permission::Shell)
            ]
        );
        assert!(words(&s).contains("Claude Code may also offer files"));
        apply(&mut s, set(&["codex", "view"])).unwrap();
        assert_eq!(s.levels.len(), 1, "View is the default, so its line goes");
        assert!(apply(&mut s, set(&["claude", "root"])).is_err());
        assert!(apply(&mut s, set(&["claude"])).is_err());
        assert!(apply(&mut s, set(&["claude", "shell", "x"])).is_err());
        assert!(apply(&mut s, set(&["Bad Name!", "shell"])).is_err());
    }

    #[test]
    fn an_assistant_name_and_a_level_parse_as_a_level() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            what: What,
        }
        let what = Cli::try_parse_from(["set", "claude", "control"])
            .unwrap()
            .what;
        let What::Level(args) = what else {
            panic!("{what:?}");
        };
        assert_eq!(args, ["claude", "control"]);
        let what = Cli::try_parse_from(["set", "ask-on-phone", "on"])
            .unwrap()
            .what;
        assert!(matches!(what, What::AskOnPhone { state: OnOff::On }));
    }

    #[test]
    fn replies_other_than_settings_are_errors() {
        assert!(settings_of(Response::Error("bad".into())).is_err());
        assert!(settings_of(Response::Ok).is_err());
        let s = AssistantSettings::default();
        assert_eq!(
            settings_of(Response::AssistantSettings(s.clone())).unwrap(),
            s
        );
    }
}
