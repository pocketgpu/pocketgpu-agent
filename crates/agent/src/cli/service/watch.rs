//! When the watcher on Windows starts the agent again. Windows has no service manager to do
//! it, as systemd (`Restart=on-failure`) and launchd (`KeepAlive`) do, so the login entry
//! starts `pocketgpu watch-agent`, which runs the agent and waits for it. A clean exit (a stop
//! asked for: `pgpu service uninstall`, an update, `pgpu relay set`) ends the watcher too;
//! anything else (a crash, an error) starts the agent again after a pause.

use std::time::Duration;

/// The first pause, as systemd's `RestartSec`.
const FIRST: Duration = Duration::from_secs(5);
/// The longest pause, for an agent that fails as soon as it starts.
const LONGEST: Duration = Duration::from_secs(60);
/// An agent that ran this long was working: its failure starts again from `FIRST`.
const STEADY: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub struct Restarts {
    /// Failures in a row of agents that ran less than `STEADY`.
    quick: u32,
}

impl Restarts {
    /// After the agent ended, having run for `ran`: how long to wait before starting it
    /// again, or `None` to stop watching.
    pub fn after_exit(&mut self, clean: bool, ran: Duration) -> Option<Duration> {
        if clean {
            return None;
        }
        if ran >= STEADY {
            self.quick = 0;
        }
        let wait = FIRST.saturating_mul(1 << self.quick.min(4)).min(LONGEST);
        self.quick = self.quick.saturating_add(1);
        Some(wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);
    const HOUR: Duration = Duration::from_secs(3600);

    /// What `start` runs on Windows, and so what the login entry ends up starting.
    #[test]
    fn the_watcher_is_a_command() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from(["pocketgpu", "watch-agent"]).unwrap();
        assert!(matches!(cli.command, Some(crate::cli::Command::WatchAgent)));
    }

    #[test]
    fn a_clean_stop_ends_the_watcher() {
        let mut r = Restarts::default();
        assert_eq!(r.after_exit(true, HOUR), None);
        assert_eq!(r.after_exit(true, SECOND), None);
    }

    #[test]
    fn a_crash_starts_the_agent_again() {
        let mut r = Restarts::default();
        assert_eq!(r.after_exit(false, HOUR), Some(FIRST));
    }

    #[test]
    fn quick_failures_wait_longer_up_to_a_minute() {
        let mut r = Restarts::default();
        let waits: Vec<u64> = (0..7)
            .map(|_| r.after_exit(false, SECOND).unwrap().as_secs())
            .collect();
        assert_eq!(waits, [5, 10, 20, 40, 60, 60, 60]);
    }

    #[test]
    fn an_agent_that_ran_a_while_starts_from_the_first_pause() {
        let mut r = Restarts::default();
        for _ in 0..5 {
            r.after_exit(false, SECOND);
        }
        assert_eq!(r.after_exit(false, HOUR), Some(FIRST));
        assert_eq!(r.after_exit(false, SECOND), Some(FIRST * 2));
    }
}
