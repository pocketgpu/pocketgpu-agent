//! Agents 1.2.23 and 1.2.24 have `GetSystemHistory` third in `control::Request`, so every
//! later request reaches them as the one before it: a CPU history as a restart or shutdown,
//! a list of runs as `Unpair`, a new saved command as running one. Only `Hello` and
//! `GetHistory` mean the same there (and their answers read the same here), so a machine on
//! one of them gets nothing else until it is updated with `pgpu update`.

use pocketgpu_proto::msg::control::Request;

/// Whether an agent of this version reads control requests shifted by one.
pub(crate) fn shifted(agent_version: &str) -> bool {
    let v = agent_version.trim().trim_start_matches('v');
    matches!(v.split(['-', '+']).next(), Some("1.2.23" | "1.2.24"))
}

/// Whether a request means the same to such an agent.
pub(crate) fn same_there(req: &Request) -> bool {
    matches!(req, Request::Hello { .. } | Request::GetHistory { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketgpu_proto::msg::control::{PowerKind, Range};

    #[test]
    fn only_the_two_shifted_releases() {
        assert!(shifted("1.2.23"));
        assert!(shifted("1.2.24"));
        assert!(shifted("v1.2.24-rc1"));
        for ok in ["1.2.22", "1.2.25", "1.2.230", "0.1.0", "1.3.0", ""] {
            assert!(!shifted(ok), "{ok}");
        }
    }

    #[test]
    fn they_get_only_hello_and_gpu_history() {
        let hello = Request::Hello {
            app_version: "1".into(),
            proto: 1,
        };
        let history = Request::GetHistory {
            range: Range::H24,
            gpu: 0,
        };
        assert!(same_there(&hello));
        assert!(same_there(&history));
        // Read there as a shutdown, as `Unpair`, and as getting runs.
        for no in [
            Request::GetSystemHistory { range: Range::H24 },
            Request::ListRuns { limit: 20 },
            Request::Unpair,
            Request::PowerAction {
                action: PowerKind::Restart,
            },
            Request::CheckUpdate,
        ] {
            assert!(!same_there(&no), "{no:?}");
        }
    }
}
