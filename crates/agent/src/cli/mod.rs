//! The `pgpu` command line. The binary is `pocketgpu`; `pgpu` is the same program.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::store::paths::Paths;

pub mod actions;
pub mod alerts;
pub mod audit;
pub mod devices;
pub mod docker;
pub mod doctor;
pub mod jobs;
pub mod notify;
mod pair;
pub mod relay;
pub mod service;
pub mod shell;
pub mod status;
pub mod update;

#[derive(Debug, Parser)]
#[command(
    name = "pgpu",
    version,
    about = "Your GPU machine, in your pocket",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the agent in the foreground (for Docker and rented GPUs).
    Agent {
        /// Windows: start the agent with no console window and return (the login entry).
        #[arg(long, hide = true)]
        background: bool,
    },
    /// Show the agent, GPUs, jobs and Ollama (the default command).
    Status,
    /// Pair a phone: shows a QR code and a one-time code, then asks to approve the phone.
    Pair {
        /// Grant this permission without asking: view, control or shell.
        #[arg(long, value_parser = parse_permission)]
        permission: Option<pocketgpu_proto::Permission>,
    },
    /// Run a command as a watched job: pgpu run [--name N] [--no-alert] -- <command>
    Run {
        /// Name shown on the phone; defaults to the script or program name.
        #[arg(long)]
        name: Option<String>,
        /// Don't send alerts when this job ends.
        #[arg(long)]
        no_alert: bool,
        /// The command and its arguments, after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// List recent jobs.
    Jobs {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Print a job's output; with -f, keep following it until the job ends.
    Logs {
        job: u64,
        #[arg(short, long)]
        follow: bool,
    },
    /// Stop a running job: SIGTERM, then SIGKILL after 10 seconds.
    Stop { job: u64 },
    /// Show or change alert rules: list, set <kind> <value>, on <kind>, off <kind>.
    Alerts {
        #[command(subcommand)]
        action: Option<alerts::Action>,
    },
    /// Quick actions the phone can run: list, add, edit, rm, icons.
    Actions {
        #[command(subcommand)]
        action: Option<actions::Command>,
    },
    /// Show paired phones, or change one's permission: devices set <device> <view|control|shell>.
    Devices {
        #[command(subcommand)]
        action: Option<devices::Command>,
    },
    /// Remove a paired phone and disconnect it at once.
    Revoke {
        /// The phone's name, or the start of its key.
        device: String,
    },
    /// Show what phones and this command line changed, newest first.
    Audit {
        /// How far back: 30m, 6h, 1d (the default) or 2w.
        #[arg(long, default_value = "1d", value_parser = audit::parse_since)]
        since: i64,
    },
    /// Check the driver, GPU access, network, Ollama, disk and user.
    Doctor,
    /// Alert channels besides push: list (the default), add, rm, test.
    Notify {
        #[command(subcommand)]
        action: Option<notify::Action>,
    },
    /// Your own iroh-relay: show (the default), set <url>, clear.
    Relay {
        #[command(subcommand)]
        action: Option<relay::Action>,
    },
    /// Docker containers for phones: status (the default), on or off.
    Docker {
        #[command(subcommand)]
        action: Option<docker::Action>,
    },
    /// Terminal access for Shell phones: status (the default), on or off.
    Shell {
        #[command(subcommand)]
        action: Option<shell::Action>,
    },
    /// Install a signed new version: checks its signature and checksum before replacing this one.
    Update {
        /// Only say whether a newer version exists.
        #[arg(long)]
        check: bool,
        /// Install this release instead of the latest, for example v1.0.1 (also goes back).
        #[arg(long, value_name = "TAG")]
        version: Option<String>,
    },
    /// Start the agent at login: a systemd user service, a launchd agent on macOS, or a login entry on Windows.
    Service {
        #[command(subcommand)]
        action: service::Action,
    },
}

async fn run_job(
    paths: &Paths,
    json: bool,
    name: &Option<String>,
    no_alert: bool,
    command: &[String],
) -> anyhow::Result<()> {
    let opts = crate::jobs::runner::RunOptions {
        name: name.clone(),
        no_alert,
        command: command.to_vec(),
    };
    crate::jobs::runner::run(paths, json, opts).await
}

/// Ends the command with this exit code; the command already printed why.
#[derive(Debug)]
pub struct Silent(pub u8);

impl std::fmt::Display for Silent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Silent {}

fn parse_permission(s: &str) -> Result<pocketgpu_proto::Permission, String> {
    s.parse()
}

/// Exit codes: 0 ok, 1 error, 2 usage (clap's own code).
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    #[cfg(windows)]
    if let Some(code) = drop_admin(&cli) {
        return code;
    }
    init_logging(matches!(
        cli.command,
        Some(Command::Agent { background: false })
    ));
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return fail(&cli, &anyhow::anyhow!("could not start: {e}")),
    };
    let result = runtime.block_on(run(&cli));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&cli, &e),
    }
}

async fn run(cli: &Cli) -> anyhow::Result<()> {
    let command = cli.command.as_ref().unwrap_or(&Command::Status);
    // Doctor runs as root too, to explain the problem.
    if !matches!(command, Command::Doctor) {
        crate::daemon::root::check()?;
    }
    let paths = Paths::discover()?;
    match command {
        Command::Agent { background: false } => crate::daemon::run(paths).await,
        Command::Agent { background: true } => background(),
        Command::Status => status::run(&paths, cli.json).await,
        Command::Pair { permission } => pair::run(&paths, cli.json, *permission).await,
        Command::Doctor => doctor::run(&paths, cli.json).await,
        Command::Alerts { action } => alerts::run(&paths, cli.json, action.clone()).await,
        Command::Actions { action } => actions::run(&paths, cli.json, action.clone()).await,
        Command::Audit { since } => audit::run(&paths, cli.json, *since).await,
        Command::Devices { action } => devices::run(&paths, cli.json, action.clone()).await,
        Command::Revoke { device } => devices::revoke(&paths, cli.json, device.clone()).await,
        Command::Run {
            name,
            no_alert,
            command,
        } => run_job(&paths, cli.json, name, *no_alert, command).await,
        Command::Jobs { limit } => jobs::list(&paths, cli.json, *limit).await,
        Command::Logs { job, follow } => jobs::logs(&paths, *job, *follow).await,
        Command::Stop { job } => jobs::stop(&paths, cli.json, *job).await,
        Command::Service { action } => service::run(*action, cli.json),
        Command::Shell { action } => shell::run(&paths, cli.json, *action).await,
        Command::Docker { action } => docker::run(&paths, cli.json, *action).await,
        Command::Notify { action } => notify::run(&paths, cli.json, action.clone()).await,
        Command::Relay { action } => relay::run(&paths, cli.json, action.clone()).await,
        Command::Update { check, version } => {
            update::run(cli.json, *check, version.as_deref()).await
        }
    }
}

fn fail(cli: &Cli, err: &anyhow::Error) -> ExitCode {
    if let Some(Silent(code)) = err.downcast_ref::<Silent>() {
        return ExitCode::from(*code);
    }
    if cli.json {
        println!("{}", serde_json::json!({ "error": format!("{err:#}") }));
    } else {
        eprintln!("error: {err:#}");
    }
    ExitCode::from(1)
}

fn init_logging(daemon: bool) {
    // Commands explain problems themselves (doctor above all); library warnings are noise there.
    let default = if daemon {
        "info"
    } else {
        "error,pocketgpu=warn,pocketgpu::collect=error"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("PGPU_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(windows)]
fn background() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    service::start_background(&exe)
}

#[cfg(not(windows))]
fn background() -> anyhow::Result<()> {
    anyhow::bail!("--background is for Windows; on Linux and macOS, use `pgpu service install`")
}

/// Started as Administrator: runs the same command again without admin rights (see
/// `winsys::unelevated`). `None` when this process may go on itself.
#[cfg(windows)]
fn drop_admin(cli: &Cli) -> Option<ExitCode> {
    use crate::winsys::unelevated;
    // A service account stays one with fewer rights; the root check then refuses it.
    if unelevated::is_child()
        || !crate::winsys::is_elevated()
        || crate::winsys::is_service_account()
    {
        return None;
    }
    // Said once where it matters; `pgpu doctor` shows it too.
    if !cli.json && matches!(cli.command, Some(Command::Agent { background: false })) {
        eprintln!("Started as Administrator; running without admin rights.");
    }
    match unelevated::run_unelevated() {
        Ok(code) => Some(ExitCode::from(u8::try_from(code).unwrap_or(1))),
        // root::check then refuses, as before.
        Err(e) => {
            eprintln!("warning: {e:#}");
            None
        }
    }
}
