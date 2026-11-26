//! Share a model (F48): one model on this computer for one person, who chats from a browser.
//!
//! A share is made by a Shell device (`control.rs`) and claimed once by the browser that
//! opens its link (`handler.rs`, `pgpu/model-guest/1`). Guests live in `model_guests`, never
//! in `devices`, so every other protocol refuses them without knowing they exist. Each chat
//! goes to the share's model with its caps (`chat.rs`); only counts are kept.
//! `specs/F48-share-a-model/contracts/model-guest.md`.

pub mod chat;
pub mod control;
pub mod day;
pub mod handler;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::PublicKey;
use pocketgpu_proto::share::{GuestState, GuestStatus, ShareInfo, ShareLinkParts, ShareState};

use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;
use store::Share;

/// Wrong proofs one key may send before it is refused, for an hour after its last one.
///
/// Wrong proofs count against their own key only. They never end a share or refuse other
/// keys: a link's secret is 32 random bytes, so no number of tries guesses it, while anyone
/// holding one old link knows this computer and can make keys for free. Counting per share or
/// for everyone would only let them end the owner's new links or turn every new guest away.
pub const WRONG_PER_KEY: u8 = 2;
/// How long a key that sent too many wrong proofs stays refused.
pub const WRONG_FOR: Duration = Duration::from_secs(3600);
/// Keys remembered for wrong proofs at most, so fresh keys can't fill memory.
const WRONG_KEYS_KEPT: usize = 4096;

/// In memory only: forgotten on restart, which only resets the counts.
#[derive(Debug, Default)]
pub struct Runtime {
    busy: Mutex<HashSet<u64>>,
    /// Each key's wrong proofs and when its last one came.
    wrong_by_key: Mutex<HashMap<PublicKey, (u8, Instant)>>,
}

impl Runtime {
    /// Marks an answer as being written for this share; `None` when one already is.
    pub fn begin(self: &Arc<Self>, share: u64) -> Option<BusyGuard> {
        let mut busy = self.busy.lock().expect("share lock poisoned");
        busy.insert(share).then(|| BusyGuard {
            runtime: self.clone(),
            share,
        })
    }

    /// Whether this key may try a claim now: only its own wrong proofs count.
    pub fn may_try(&self, key: &PublicKey) -> bool {
        let by_key = self.wrong_by_key.lock().expect("share lock poisoned");
        !by_key
            .get(key)
            .is_some_and(|(n, last)| *n >= WRONG_PER_KEY && last.elapsed() < WRONG_FOR)
    }

    /// Counts a wrong proof against this key, and nothing else (see [`WRONG_PER_KEY`]).
    pub fn wrong(&self, key: PublicKey) {
        let mut by_key = self.wrong_by_key.lock().expect("share lock poisoned");
        let now = Instant::now();
        if let Some((n, last)) = by_key.get_mut(&key) {
            if last.elapsed() >= WRONG_FOR {
                *n = 0;
            }
            *n = n.saturating_add(1);
            *last = now;
            return;
        }
        if by_key.len() >= WRONG_KEYS_KEPT {
            by_key.retain(|_, (_, last)| last.elapsed() < WRONG_FOR);
        }
        // Still full: the key whose last wrong proof is oldest makes room, so every key is
        // counted and refused after its second.
        if by_key.len() >= WRONG_KEYS_KEPT
            && let Some(oldest) = by_key
                .iter()
                .min_by_key(|(_, (_, last))| *last)
                .map(|(k, _)| *k)
        {
            by_key.remove(&oldest);
        }
        by_key.insert(key, (1, now));
    }
}

/// Clears the share's busy mark when the answer ends, however it ends.
#[derive(Debug)]
pub struct BusyGuard {
    runtime: Arc<Runtime>,
    share: u64,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.runtime
            .busy
            .lock()
            .expect("share lock poisoned")
            .remove(&self.share);
    }
}

/// The relays a link carries: this computer's, else the release's, at most three.
pub fn link_relays(agent: &Agent) -> Vec<String> {
    let mut relays = agent
        .config
        .read()
        .expect("config lock poisoned")
        .relays
        .clone();
    if relays.is_empty() {
        relays = pocketgpu_proto::default_relays()
            .into_iter()
            .map(String::from)
            .collect();
    }
    relays.truncate(pocketgpu_proto::share::MAX_RELAYS);
    relays
}

/// Ends a share now: its guest is forgotten and its connections close.
pub fn end(agent: &Agent, share: u64, by: &str, why: &str) {
    let guest = {
        let db = agent.db();
        let guest = store::guest_of_share(&db, share).ok().flatten();
        let _ = store::remove_guest(&db, share);
        let _ = store::set_state(&db, share, ShareState::Ended, now_ms());
        if let Ok(Some(s)) = store::get(&db, share) {
            let _ = audit::record(&db, by, Event::Share, &target(&s), why);
        }
        guest
    };
    if let Some(key) = guest.and_then(|k| k.parse::<PublicKey>().ok()) {
        agent.drop_connections(&key);
    }
}

/// What the audit log names: the model and the share, never anything said.
pub fn target(s: &Share) -> String {
    format!("{} (share {})", s.model, s.id)
}

/// Whether a job runs now and the share pauses for jobs.
pub fn paused(agent: &Agent, s: &Share) -> bool {
    s.pause_for_jobs
        && crate::jobs::store::running_ids(&agent.db())
            .map(|ids| !ids.is_empty())
            .unwrap_or(false)
}

/// The share is over, by state or by time.
pub fn is_over(s: &Share, now: i64) -> bool {
    s.state == ShareState::Ended || s.ends_ms.is_some_and(|e| e <= now)
}

pub fn used_today(agent: &Agent, s: &Share, now: i64) -> u32 {
    store::used_on(&agent.db(), s.id, &day::local_day(now)).unwrap_or(0)
}

/// What the guest sees about its share now.
pub fn guest_status(agent: &Agent, s: &Share, now: i64) -> GuestStatus {
    let used = used_today(agent, s, now);
    let state = if is_over(s, now) {
        GuestState::Ended
    } else if paused(agent, s) {
        GuestState::Paused
    } else if s.daily_limit.is_some_and(|l| used >= l) {
        GuestState::LimitReached {
            resets_ms: day::next_local_midnight_ms(now),
        }
    } else {
        GuestState::Ready
    };
    GuestStatus {
        computer: agent.name(),
        from: s.from_name.clone(),
        model: s.model.clone(),
        ends_ms: s.ends_ms,
        limit: s.daily_limit,
        used_today: used,
        state,
    }
}

/// What the owner sees about a share: counts, never text; the link only while it waits.
pub fn info(agent: &Agent, s: &Share, now: i64) -> ShareInfo {
    let state = if is_over(s, now) {
        ShareState::Ended
    } else {
        s.state
    };
    ShareInfo {
        id: s.id,
        label: s.label.clone(),
        from: s.from_name.clone(),
        model: s.model.clone(),
        server: s.server,
        state,
        paused: state != ShareState::Ended && paused(agent, s),
        used_today: used_today(agent, s, now),
        limit: s.daily_limit,
        ends_ms: s.ends_ms,
        pause_for_jobs: s.pause_for_jobs,
        created_ms: s.created_ms,
        link: (state == ShareState::Waiting).then(|| ShareLinkParts {
            share: s.id,
            secret: s.secret,
            relays: link_relays(agent),
        }),
    }
}

/// Ends shares whose time is up and forgets old ones. Returns how many ended.
pub fn sweep(agent: &Agent) -> usize {
    let now = now_ms();
    let due = store::due(&agent.db(), now).unwrap_or_default();
    for id in &due {
        end(agent, *id, "local", "ended");
    }
    let old_day = day::local_day(now - 31 * 86_400_000);
    let _ = store::purge(&agent.db(), now, &old_day);
    due.len()
}

/// Sweeps while the agent runs: when the next share's time is up, and every minute besides.
pub fn start_sweeper(agent: Arc<Agent>) {
    tokio::spawn(async move {
        loop {
            sweep(&agent);
            let soonest = store::soonest_due(&agent.db()).ok().flatten();
            let wait = soonest.map_or(Duration::from_secs(60), |t| {
                Duration::from_millis(u64::try_from(t - now_ms()).unwrap_or(0))
                    .clamp(Duration::from_secs(1), Duration::from_secs(60))
            });
            tokio::time::sleep(wait).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn key() -> PublicKey {
        SecretKey::generate().public()
    }

    #[test]
    fn wrong_proofs_stop_only_their_own_key() {
        let rt = Runtime::default();
        // Far more wrong proofs than any old limit, each pair from a fresh key.
        for _ in 0..200 {
            let k = key();
            assert!(rt.may_try(&k));
            rt.wrong(k);
            rt.wrong(k);
            assert!(!rt.may_try(&k), "a key with 2 wrong proofs may try again");
        }
        assert!(
            rt.may_try(&key()),
            "other keys' wrong proofs turned a new guest away"
        );
    }

    #[test]
    fn a_refused_key_may_try_again_after_an_hour() {
        let rt = Runtime::default();
        let k = key();
        let Some(long_ago) = Instant::now().checked_sub(WRONG_FOR) else {
            return; // this clock can't go back an hour yet
        };
        rt.wrong_by_key
            .lock()
            .unwrap()
            .insert(k, (WRONG_PER_KEY, long_ago));
        assert!(rt.may_try(&k));
        rt.wrong(k);
        assert!(rt.may_try(&k), "an old count wasn't started again");
    }

    #[test]
    fn fresh_keys_cannot_fill_memory() {
        let rt = Runtime::default();
        for _ in 0..WRONG_KEYS_KEPT + 50 {
            rt.wrong(key());
        }
        assert!(rt.wrong_by_key.lock().unwrap().len() <= WRONG_KEYS_KEPT);
    }

    #[test]
    fn a_full_list_still_refuses_a_new_key() {
        let rt = Runtime::default();
        let now = Instant::now();
        let Some(earlier) = now.checked_sub(Duration::from_secs(1)) else {
            return; // this clock can't go back a second yet
        };
        let first = key();
        {
            let mut by_key = rt.wrong_by_key.lock().unwrap();
            // The oldest, still within its hour, so only eviction makes room.
            by_key.insert(first, (1, earlier));
            while by_key.len() < WRONG_KEYS_KEPT {
                by_key.insert(key(), (1, now));
            }
        }
        let k = key();
        rt.wrong(k);
        rt.wrong(k);
        assert!(
            !rt.may_try(&k),
            "a new key wasn't counted once the list was full"
        );
        let by_key = rt.wrong_by_key.lock().unwrap();
        assert_eq!(by_key.len(), WRONG_KEYS_KEPT);
        assert!(
            !by_key.contains_key(&first),
            "the oldest key wasn't the one dropped"
        );
    }
}
