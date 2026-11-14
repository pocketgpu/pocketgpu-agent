//! Invites (F44, board 57): a device with Shell lends the computer for a while. An invite is
//! a one-time pairing code made on the phone's request and approved in advance, with the
//! level and the end time fixed when it is made: whoever proves it within
//! [`JOIN_WITHIN`] pairs at once, with no question at the computer. The device's access
//! ends by itself at the end time (`devices.expires_ms`, checked on every request), and the
//! sweeper removes it then ([`sweep`]). Invites live in memory only: a restart forgets the
//! ones not used yet.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use iroh::PublicKey;
use pocketgpu_proto::Permission;
use pocketgpu_proto::pairing::{PairCode, proof, proof_matches};

/// How long an invite may be used to join.
pub const JOIN_WITHIN: Duration = Duration::from_secs(15 * 60);
/// The longest a guest may have: a week.
pub const MAX_MINUTES: u32 = 7 * 24 * 60;
/// Invites not used yet, at most.
const MAX_OPEN: usize = 8;
/// Wrong proofs before every open invite stops working (as for pairing codes).
pub const MAX_WRONG: u8 = 8;
/// Wrong proofs one key may send; later ones are refused without counting.
pub const MAX_WRONG_PER_KEY: usize = 2;

/// What a guest gets once it joins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub permission: Permission,
    pub ends_ms: i64,
    /// How long it was given for, for the words ("for 8 hours").
    pub minutes: u32,
    /// The name of the device that made the invite.
    pub by: String,
}

#[derive(Debug)]
struct Invite {
    secret: [u8; 32],
    grant: Grant,
    join_until: Instant,
}

#[derive(Debug, Default)]
struct Open {
    invites: Vec<Invite>,
    /// Wrong proofs since the last invite was made, and the key behind each.
    wrong: Vec<PublicKey>,
}

#[derive(Debug, Default)]
pub struct Invites(Mutex<Open>);

impl Invites {
    fn open(&self) -> std::sync::MutexGuard<'_, Open> {
        self.0.lock().expect("invites lock poisoned")
    }

    /// Keeps a new invite; its code's secret is slow to make (Argon2id), so the caller makes
    /// it off the async threads.
    pub fn add(&self, secret: [u8; 32], grant: Grant) -> Result<(), String> {
        let mut open = self.open();
        let now = Instant::now();
        // A new invite starts the count of wrong proofs again.
        open.wrong.clear();
        let list = &mut open.invites;
        list.retain(|i| i.join_until > now);
        if list.len() >= MAX_OPEN {
            return Err(format!(
                "{MAX_OPEN} invites are waiting to be used already; wait 15 minutes"
            ));
        }
        list.push(Invite {
            secret,
            grant,
            join_until: now + JOIN_WITHIN,
        });
        Ok(())
    }

    /// Whether any invite may still be used: unknown keys may then try `pgpu/pair/1`.
    pub fn any_open(&self) -> bool {
        let now = Instant::now();
        self.open().invites.iter().any(|i| i.join_until > now)
    }

    /// The grant of the invite this proof is for, used up so it works once.
    pub fn take(
        &self,
        endpoint: &PublicKey,
        device: &PublicKey,
        given: &[u8; 32],
    ) -> Option<Grant> {
        let mut open = self.open();
        if open.wrong.iter().filter(|k| *k == device).count() >= MAX_WRONG_PER_KEY {
            return None;
        }
        let now = Instant::now();
        let list = &mut open.invites;
        list.retain(|i| i.join_until > now);
        let at = list.iter().position(|i| {
            let expected = proof(&i.secret, endpoint.as_bytes(), device.as_bytes());
            proof_matches(&expected, given)
        })?;
        Some(list.remove(at).grant)
    }

    /// A proof that matched no invite and no pairing code: after [`MAX_WRONG`] of them,
    /// every open invite stops working, so guessing one is as hard as guessing a code.
    pub fn missed(&self, device: &PublicKey) {
        let mut open = self.open();
        if open.invites.is_empty() {
            return;
        }
        open.wrong.push(*device);
        if open.wrong.len() >= usize::from(MAX_WRONG) {
            open.invites.clear();
            open.wrong.clear();
        }
    }
}

/// A new code for an invite, from the system's random source.
pub fn new_code() -> anyhow::Result<PairCode> {
    let mut b = [0u8; 5];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no random source: {e}"))?;
    Ok(PairCode::from_random(b))
}

/// "for 8 hours", "for 1 day", for notices and the audit log.
pub fn for_words(minutes: u32) -> String {
    let (n, unit) = if minutes.is_multiple_of(24 * 60) {
        (minutes / (24 * 60), "day")
    } else if minutes.is_multiple_of(60) {
        (minutes / 60, "hour")
    } else {
        (minutes, "minute")
    };
    format!("for {n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> Grant {
        Grant {
            permission: Permission::Control,
            ends_ms: 1,
            minutes: 60,
            by: "Sam's iPhone".into(),
        }
    }

    #[test]
    fn an_invite_works_once_for_its_proof() {
        let invites = Invites::default();
        let secret = [7u8; 32];
        invites.add(secret, grant()).unwrap();
        let endpoint = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let device = iroh::SecretKey::from_bytes(&[2; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let good = proof(&secret, endpoint.as_bytes(), device.as_bytes());
        // Bound to the device: another one can't use the same proof.
        assert!(invites.take(&endpoint, &other, &good).is_none());
        assert!(invites.any_open());
        assert_eq!(invites.take(&endpoint, &device, &good), Some(grant()));
        assert!(invites.take(&endpoint, &device, &good).is_none());
        assert!(!invites.any_open());
    }

    #[test]
    fn wrong_proofs_close_the_invites() {
        let invites = Invites::default();
        let secret = [7u8; 32];
        invites.add(secret, grant()).unwrap();
        let endpoint = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let device = iroh::SecretKey::from_bytes(&[2; 32]).public();
        let good = proof(&secret, endpoint.as_bytes(), device.as_bytes());
        // One key's wrong tries stop counting after two, and lock that key out.
        let guesser = iroh::SecretKey::from_bytes(&[9; 32]).public();
        invites.missed(&guesser);
        invites.missed(&guesser);
        let theirs = proof(&secret, endpoint.as_bytes(), guesser.as_bytes());
        assert!(invites.take(&endpoint, &guesser, &theirs).is_none());
        // Many keys guessing: every invite closes.
        for i in 0..MAX_WRONG {
            invites.missed(&iroh::SecretKey::from_bytes(&[20 + i; 32]).public());
        }
        assert!(!invites.any_open());
        assert!(invites.take(&endpoint, &device, &good).is_none());
    }

    #[test]
    fn only_a_few_wait_at_once() {
        let invites = Invites::default();
        for i in 0..MAX_OPEN {
            invites.add([i as u8; 32], grant()).unwrap();
        }
        assert!(invites.add([99; 32], grant()).is_err());
    }

    #[test]
    fn says_how_long() {
        assert_eq!(for_words(60), "for 1 hour");
        assert_eq!(for_words(8 * 60), "for 8 hours");
        assert_eq!(for_words(24 * 60), "for 1 day");
        assert_eq!(for_words(7 * 24 * 60), "for 7 days");
        assert_eq!(for_words(90), "for 90 minutes");
    }
}
