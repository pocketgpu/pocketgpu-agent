//! Share a model (F48): one model on one computer for one person, chatting from a browser.
//!
//! The link is `https://share.pocketgpu.app/#<fragment>`: everything after `#` stays in the
//! browser. The guest page claims the share once on `pgpu/model-guest/1` with a proof of the
//! link's secret bound to its own key, then may only ask for its status and chat with the
//! share's model (`specs/F48-share-a-model/contracts/model-guest.md`).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

/// The guest page's address; the fragment follows `#`.
pub const PAGE: &str = "https://share.pocketgpu.app/";
/// Messages the page may send as context with one chat message.
pub const MAX_MESSAGES: usize = 40;
/// Bytes in one message's text.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;
/// Bytes in all the messages of one chat request.
pub const MAX_TOTAL_BYTES: usize = 64 * 1024;
/// The longest answer, in the model's tokens.
pub const ANSWER_TOKENS: u32 = 2048;
/// How long a link may wait to be opened when its share doesn't end sooner.
pub const CLAIM_WITHIN_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Shares not ended, at most, on one computer: a guard, not a plan limit.
pub const MAX_OPEN_SHARES: usize = 20;
/// The daily limits the owner can pick; `None` is no limit.
pub const LIMITS: [u32; 3] = [50, 200, 500];
/// The lengths the owner can pick, in minutes (a day, a week, a month); `None` is until stopped.
pub const MINUTES: [u32; 3] = [1440, 10080, 43200];
/// Relays carried in a link, at most.
pub const MAX_RELAYS: usize = 3;
/// The longest fragment accepted.
pub const MAX_FRAGMENT: usize = 600;
/// Characters in the owner's name for the person, and in the name shown to them.
pub const MAX_NAME: usize = 40;

/// What a link carries. Only version 1 exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareLink {
    pub v: u8,
    /// The computer's endpoint key.
    pub computer: [u8; 32],
    pub share: u64,
    pub secret: [u8; 32],
    /// Relay URLs to reach the computer through, most preferred first.
    pub relays: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BadLink {
    #[error("this link is too long")]
    TooLong,
    #[error("this link is damaged")]
    Damaged,
    #[error("this link is for a newer PocketGPU")]
    Version,
    #[error("this link has no way to reach the computer")]
    NoRelay,
}

impl ShareLink {
    pub fn to_fragment(&self) -> String {
        let bytes = postcard::to_stdvec(self).expect("a link always encodes");
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub fn from_fragment(fragment: &str) -> Result<Self, BadLink> {
        let fragment = fragment.trim_start_matches('#').trim();
        if fragment.len() > MAX_FRAGMENT {
            return Err(BadLink::TooLong);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(fragment)
            .map_err(|_| BadLink::Damaged)?;
        let link: Self = postcard::from_bytes(&bytes).map_err(|_| BadLink::Damaged)?;
        if link.v != 1 {
            return Err(BadLink::Version);
        }
        if link.relays.is_empty() {
            return Err(BadLink::NoRelay);
        }
        if link.relays.len() > MAX_RELAYS {
            return Err(BadLink::Damaged);
        }
        Ok(link)
    }

    pub fn url(&self) -> String {
        format!("{PAGE}#{}", self.to_fragment())
    }
}

/// The model server a share uses: Ollama, or another one by its port on the computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelServerRef {
    Ollama,
    Port(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMsg {
    pub role: Role,
    pub text: String,
}

/// Guest → computer, one per stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuestToAgent {
    /// `proof` is `pairing::proof(secret, computer key, guest key)`.
    Claim {
        share: u64,
        proof: [u8; 32],
    },
    /// What the share is, before claiming it (the welcome page): the same proof, nothing
    /// claimed, so a link preview that runs the page can't use the link up. Answers `Status`.
    Peek {
        share: u64,
        proof: [u8; 32],
    },
    Status,
    Chat {
        messages: Vec<ChatMsg>,
    },
}

/// What the guest can do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuestState {
    Ready,
    /// A job runs on the computer and the owner asked for sharing to pause then.
    Paused,
    LimitReached {
        resets_ms: i64,
    },
    Ended,
    ModelUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestStatus {
    /// The computer's name.
    pub computer: String,
    /// The owner's name for the guest to see, if they gave one.
    pub from: Option<String>,
    pub model: String,
    pub ends_ms: Option<i64>,
    pub limit: Option<u32>,
    pub used_today: u32,
    pub state: GuestState,
}

/// Why the computer said no.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuestRefusal {
    /// This key has no share here.
    NotForYou,
    AlreadyClaimed,
    LinkExpired,
    Ended,
    LimitReached {
        resets_ms: i64,
    },
    Paused,
    ModelUnavailable,
    TooLong,
    /// An answer is still being written for this guest.
    Busy,
    TooManyTries,
}

/// Computer → guest. A chat streams `Text`s, then `Speed` (newer computers), then `Done`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentToGuest {
    Claimed(GuestStatus),
    Status(GuestStatus),
    Text(String),
    Done {
        used_today: u32,
    },
    Refused(GuestRefusal),
    /// How fast the answer came, sent just before `Done`: tokens a second (rounded) and the
    /// answer's whole time. Last, so pages from before it still read everything else.
    Speed {
        tokens_per_s: u32,
        ms: u32,
    },
}

/// A share as its owner sees it: counts, never text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareInfo {
    pub id: u64,
    pub label: String,
    pub from: Option<String>,
    pub model: String,
    pub server: ModelServerRef,
    pub state: ShareState,
    /// A job runs and the share pauses for jobs.
    pub paused: bool,
    pub used_today: u32,
    pub limit: Option<u32>,
    pub ends_ms: Option<i64>,
    pub pause_for_jobs: bool,
    pub created_ms: i64,
    /// What the app needs to show the link again; only while the share waits to be opened.
    pub link: Option<ShareLinkParts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShareState {
    Waiting,
    Active,
    Ended,
}

/// The parts of a link only the computer knows; the app adds the computer's key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareLinkParts {
    pub share: u64,
    pub secret: [u8; 32],
    pub relays: Vec<String>,
}

impl ShareLinkParts {
    pub fn link(&self, computer: [u8; 32]) -> ShareLink {
        ShareLink {
            v: 1,
            computer,
            share: self.share,
            secret: self.secret,
            relays: self.relays.clone(),
        }
    }
}

/// What the owner asks for (`control::Request::CreateShare`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewShare {
    pub server: ModelServerRef,
    pub model: String,
    pub label: String,
    pub from: Option<String>,
    pub minutes: Option<u32>,
    pub daily_limit: Option<u32>,
    pub pause_for_jobs: bool,
}

impl NewShare {
    /// Checks the choices the screens offer; anything else is refused.
    pub fn check(&self) -> Result<(), &'static str> {
        let label = self.label.trim();
        if label.is_empty() || label.chars().count() > MAX_NAME {
            return Err("the name must have 1 to 40 characters");
        }
        if let Some(f) = &self.from
            && f.trim().chars().count() > MAX_NAME
        {
            return Err("your name must have at most 40 characters");
        }
        if self.model.trim().is_empty() || self.model.len() > 200 {
            return Err("no model was chosen");
        }
        if let Some(m) = self.minutes
            && !MINUTES.contains(&m)
        {
            return Err("a share lasts a day, a week, a month or until stopped");
        }
        if let Some(l) = self.daily_limit
            && !LIMITS.contains(&l)
        {
            return Err("the daily limit is 50, 200, 500 or none");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> ShareLink {
        ShareLink {
            v: 1,
            computer: [7; 32],
            share: 42,
            secret: [9; 32],
            relays: vec!["https://relay.pocketgpu.app/".into()],
        }
    }

    #[test]
    fn a_link_round_trips_through_its_fragment() {
        let l = link();
        let url = l.url();
        assert!(url.starts_with("https://share.pocketgpu.app/#"));
        let frag = url.split('#').nth(1).unwrap();
        assert!(frag.len() < 200, "{}", frag.len());
        assert_eq!(ShareLink::from_fragment(frag).unwrap(), l);
        assert_eq!(ShareLink::from_fragment(&format!("#{frag}")).unwrap(), l);
    }

    #[test]
    fn bad_links_are_refused() {
        let mut l = link();
        l.v = 2;
        assert_eq!(
            ShareLink::from_fragment(&l.to_fragment()),
            Err(BadLink::Version)
        );
        let mut l = link();
        l.relays = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        assert_eq!(
            ShareLink::from_fragment(&l.to_fragment()),
            Err(BadLink::Damaged)
        );
        let mut l = link();
        l.relays.clear();
        assert_eq!(
            ShareLink::from_fragment(&l.to_fragment()),
            Err(BadLink::NoRelay)
        );
        assert_eq!(
            ShareLink::from_fragment(&"A".repeat(601)),
            Err(BadLink::TooLong)
        );
        assert_eq!(
            ShareLink::from_fragment("not*base64"),
            Err(BadLink::Damaged)
        );
    }

    #[test]
    fn only_the_offered_choices_are_accepted() {
        let ok = NewShare {
            server: ModelServerRef::Ollama,
            model: "llama3.3:70b".into(),
            label: "Mum".into(),
            from: Some("Kami".into()),
            minutes: Some(10080),
            daily_limit: Some(200),
            pause_for_jobs: true,
        };
        assert!(ok.check().is_ok());
        assert!(
            NewShare {
                minutes: Some(60),
                ..ok.clone()
            }
            .check()
            .is_err()
        );
        assert!(
            NewShare {
                daily_limit: Some(1000),
                ..ok.clone()
            }
            .check()
            .is_err()
        );
        assert!(
            NewShare {
                label: " ".into(),
                ..ok.clone()
            }
            .check()
            .is_err()
        );
        assert!(
            NewShare {
                label: "x".repeat(41),
                ..ok.clone()
            }
            .check()
            .is_err()
        );
        assert!(
            NewShare {
                minutes: None,
                daily_limit: None,
                from: None,
                ..ok
            }
            .check()
            .is_ok()
        );
    }
}
