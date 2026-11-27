//! What a hook hears when a held action ends without a phone answer (F32, research §3),
//! and the hash that tells one call from another.

use pocketgpu_proto::assistants::{Answer, Ended};

/// What a held "Ask me first" action hears after 5 minutes with no answer.
pub const NO_ANSWER: &str = "No answer on your phone in 5 minutes";
/// What it hears when the phone couldn't be asked and no desk prompt would show.
pub const COULDNT_ASK: &str = "PocketGPU couldn't ask your phone";
/// What a denied action hears.
pub const DENIED: &str = "Denied on your phone";
/// Allow is refused for a request whose command the phone couldn't show whole.
pub const TOO_LONG: &str = "Too long to check on the phone; answer at the computer";

pub type Hash = [u8; 32];

/// blake3 of the tool and its whole input: the same call has the same hash.
pub fn input_hash(tool: &str, input: &str) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(tool.as_bytes());
    h.update(&[0]);
    h.update(input.as_bytes());
    *h.finalize().as_bytes()
}

/// The answer when "Ask me first" couldn't reach the phone: the desk prompt where one
/// really shows (Claude Code's default and plan modes, Cursor), else a deny, since an `ask`
/// in an auto mode could be settled without the owner.
pub fn fail_closed(assistant: &str, permission_mode: &str, why: Ended) -> Answer {
    if desk_prompt_shows(assistant, permission_mode) {
        Answer::NoDecision { why }
    } else {
        Answer::Deny {
            message: COULDNT_ASK.into(),
        }
    }
}

/// Whether an `ask` from a "Ask me first" hook really shows a prompt at the desk: Cursor's
/// always does, Claude Code's only in its default and plan modes. Elsewhere an `ask` could
/// be settled without the owner, so the hook denies instead.
pub fn desk_prompt_shows(assistant: &str, permission_mode: &str) -> bool {
    assistant == "cursor" || matches!(permission_mode, "default" | "plan")
}
