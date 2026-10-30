//! The active pairing code: one at a time, single use, 5 minutes, burned after wrong proofs.

use std::time::{Duration, Instant};

use anyhow::Result;
use iroh::PublicKey;
use pocketgpu_proto::pairing::{self, PairCode};
use pocketgpu_proto::{Permission, Platform};
use tokio::sync::{mpsc, oneshot};

pub const LIFETIME: Duration = Duration::from_secs(5 * 60);
/// Wrong proofs before the code stops working.
pub const MAX_WRONG: u8 = 8;
/// Wrong proofs one key may send; later ones are refused without counting.
pub const MAX_WRONG_PER_KEY: usize = 2;

/// A phone that proved the code and now waits for someone at the machine.
#[derive(Debug)]
pub struct Ask {
    pub device: PublicKey,
    pub device_name: String,
    pub platform: Platform,
    pub push_ticket: Option<String>,
    /// The permission to grant, or `None` to deny.
    pub decision: oneshot::Sender<Option<Permission>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("no pairing code is active. Run `pgpu pair` on the machine")]
    NoCode,
    #[error("the code has expired. Run `pgpu pair` again")]
    Expired,
    #[error("the code was already used. Run `pgpu pair` again")]
    Used,
    #[error("wrong code. Check the code shown by `pgpu pair`")]
    Wrong,
    #[error("too many wrong codes. Run `pgpu pair` again")]
    Burned,
}

#[derive(Debug)]
pub struct ActiveCode {
    pub code: PairCode,
    secret: [u8; 32],
    pub expires_at: Instant,
    used: bool,
    wrong: u8,
    /// The key behind each wrong proof.
    tries: Vec<PublicKey>,
    asks: mpsc::Sender<Ask>,
}

impl ActiveCode {
    /// A fresh random code that sends phones proving it to `asks`.
    pub fn generate(asks: mpsc::Sender<Ask>) -> Result<Self> {
        let mut bytes = [0u8; 5];
        getrandom::fill(&mut bytes)
            .map_err(|e| anyhow::anyhow!("no random numbers available: {e}"))?;
        Ok(Self::new(
            PairCode::from_random(bytes),
            asks,
            Instant::now(),
        ))
    }

    pub fn new(code: PairCode, asks: mpsc::Sender<Ask>, now: Instant) -> Self {
        Self {
            code,
            secret: code.secret(),
            expires_at: now + LIFETIME,
            used: false,
            wrong: 0,
            tries: Vec::new(),
            asks,
        }
    }

    /// Whether unknown keys may still try this code.
    pub fn is_open(&self, now: Instant) -> bool {
        now < self.expires_at && !self.used && self.wrong < MAX_WRONG
    }

    /// Checks a phone's proof for the endpoint it dialed. A correct proof uses up the code.
    pub fn check(
        &mut self,
        now: Instant,
        endpoint: &PublicKey,
        device: &PublicKey,
        proof: &[u8; 32],
    ) -> Result<mpsc::Sender<Ask>, Refusal> {
        if now >= self.expires_at {
            return Err(Refusal::Expired);
        }
        if self.used {
            return Err(Refusal::Used);
        }
        if self.wrong >= MAX_WRONG {
            return Err(Refusal::Burned);
        }
        // One key alone can't burn the code: past its own tries it is just refused.
        let tries = self.tries.iter().filter(|k| *k == device).count();
        if tries >= MAX_WRONG_PER_KEY {
            return Err(Refusal::Wrong);
        }
        let expected = pairing::proof(&self.secret, endpoint.as_bytes(), device.as_bytes());
        if !pairing::proof_matches(&expected, proof) {
            self.wrong += 1;
            self.tries.push(*device);
            return Err(if self.wrong >= MAX_WRONG {
                Refusal::Burned
            } else {
                Refusal::Wrong
            });
        }
        self.used = true;
        Ok(self.asks.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn setup() -> (ActiveCode, PublicKey, PublicKey, Instant) {
        let (tx, _rx) = mpsc::channel(1);
        let now = Instant::now();
        let code = ActiveCode::new(PairCode::parse("4F7K92QD").unwrap(), tx, now);
        let agent = SecretKey::generate().public();
        let phone = SecretKey::generate().public();
        (code, agent, phone, now)
    }

    fn proof_for(code: &ActiveCode, agent: &PublicKey, phone: &PublicKey) -> [u8; 32] {
        pairing::proof(&code.code.secret(), agent.as_bytes(), phone.as_bytes())
    }

    #[test]
    fn right_proof_works_once() {
        let (mut code, agent, phone, now) = setup();
        let proof = proof_for(&code, &agent, &phone);
        assert!(code.check(now, &agent, &phone, &proof).is_ok());
        assert!(!code.is_open(now));
        assert_eq!(
            code.check(now, &agent, &phone, &proof).unwrap_err(),
            Refusal::Used
        );
    }

    #[test]
    fn proof_for_another_endpoint_or_phone_fails() {
        let (mut code, agent, phone, now) = setup();
        let other = SecretKey::generate().public();
        let proof = proof_for(&code, &agent, &phone);
        assert_eq!(
            code.check(now, &other, &phone, &proof).unwrap_err(),
            Refusal::Wrong
        );
        assert_eq!(
            code.check(now, &agent, &other, &proof).unwrap_err(),
            Refusal::Wrong
        );
    }

    #[test]
    fn expires_after_five_minutes() {
        let (mut code, agent, phone, now) = setup();
        let proof = proof_for(&code, &agent, &phone);
        let later = now + LIFETIME;
        assert!(!code.is_open(later));
        assert_eq!(
            code.check(later, &agent, &phone, &proof).unwrap_err(),
            Refusal::Expired
        );
    }

    #[test]
    fn one_key_cannot_burn_the_code() {
        let (mut code, agent, phone, now) = setup();
        let pest = SecretKey::generate().public();
        for _ in 0..20 {
            let err = code.check(now, &agent, &pest, &[0; 32]).unwrap_err();
            assert_eq!(err, Refusal::Wrong);
        }
        let proof = proof_for(&code, &agent, &phone);
        assert!(code.check(now, &agent, &phone, &proof).is_ok());
    }

    #[test]
    fn burned_after_too_many_wrong_proofs() {
        let (mut code, agent, phone, now) = setup();
        for i in 1..=MAX_WRONG {
            let guesser = SecretKey::generate().public();
            let err = code.check(now, &agent, &guesser, &[0; 32]).unwrap_err();
            let want = if i == MAX_WRONG {
                Refusal::Burned
            } else {
                Refusal::Wrong
            };
            assert_eq!(err, want);
        }
        let proof = proof_for(&code, &agent, &phone);
        assert_eq!(
            code.check(now, &agent, &phone, &proof).unwrap_err(),
            Refusal::Burned
        );
    }
}
