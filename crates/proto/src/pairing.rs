//! Pairing codes, the keys derived from them, and the QR payload.
//!
//! The 8-character code is the only secret. Both sides derive two things from it:
//! - the pairing secret, which keys the phone's proof (`secret_proof`), and
//! - a rendezvous key: while the code is active the agent also listens on an endpoint with
//!   this key, so a phone that only has the typed code can find the machine.
//!
//! Both come from the code stretched with Argon2id: the rendezvous key is public (relays and
//! address lookup see it), so it must not let anyone test codes offline faster than the
//! 5 minutes a code lives. Online, a code works once, is burned after a few wrong proofs,
//! and the person at the machine still has to approve the phone. Both sides then show
//! 64-bit key fingerprints.

use std::fmt;

/// Crockford base32, without I, L, O and U.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
// v2: derived from the Argon2id-stretched code (v1 hashed the bare code).
const SECRET_CONTEXT: &str = "pocketgpu 2026-10 pairing secret v2";
const RENDEZVOUS_CONTEXT: &str = "pocketgpu 2026-10 pairing rendezvous v2";
const STRETCH_SALT: &[u8] = b"pocketgpu pairing code 2026-10";
/// Argon2id with 19 MiB and 2 passes (OWASP's minimum): about 50-100 ms on a phone, once
/// per pairing; for an offline search of 2^40 codes, centuries.
const STRETCH_KIB: u32 = 19 * 1024;
const STRETCH_PASSES: u32 = 2;
const QR_PREFIX: &str = "pocketgpu://pair?";

/// A one-time pairing code such as `4F7K-92QD`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairCode([u8; 8]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BadCode {
    #[error("the code has 8 characters, like 4F7K-92QD")]
    Length,
    #[error("{0:?} can't be in a code")]
    Char(char),
}

impl PairCode {
    /// A code from 40 random bits.
    pub fn from_random(bytes: [u8; 5]) -> Self {
        let bits = bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        let mut out = [0u8; 8];
        for (i, c) in out.iter_mut().enumerate() {
            *c = ALPHABET[((bits >> (35 - 5 * i)) & 31) as usize];
        }
        Self(out)
    }

    /// Accepts what people type: any case, with or without dashes and spaces, and the
    /// look-alikes O for 0 and I or L for 1.
    pub fn parse(input: &str) -> Result<Self, BadCode> {
        let mut out = [0u8; 8];
        let mut n = 0;
        for ch in input.chars().filter(|c| !matches!(c, '-' | ' ')) {
            let c = match ch.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
                return Err(BadCode::Char(ch));
            }
            if n == 8 {
                return Err(BadCode::Length);
            }
            out[n] = c as u8;
            n += 1;
        }
        if n != 8 {
            return Err(BadCode::Length);
        }
        Ok(Self(out))
    }

    /// The 8 characters without the dash.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("codes are ASCII")
    }

    /// Key for the phone's proof. Slow (Argon2id): keep it rather than call it again.
    pub fn secret(&self) -> [u8; 32] {
        blake3::derive_key(SECRET_CONTEXT, &self.stretched())
    }

    /// Secret key bytes of the endpoint the agent listens on while this code is active.
    /// Slow (Argon2id).
    pub fn rendezvous_seed(&self) -> [u8; 32] {
        blake3::derive_key(RENDEZVOUS_CONTEXT, &self.stretched())
    }

    fn stretched(&self) -> [u8; 32] {
        use argon2::{Algorithm, Argon2, Params, Version};
        let params =
            Params::new(STRETCH_KIB, STRETCH_PASSES, 1, Some(32)).expect("valid parameters");
        let mut out = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(&self.0, STRETCH_SALT, &mut out)
            .expect("Argon2id with valid parameters");
        out
    }
}

impl fmt::Display for PairCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.as_str();
        write!(f, "{}-{}", &s[..4], &s[4..])
    }
}

impl fmt::Debug for PairCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairCode(…)")
    }
}

/// The phone's proof that it knows the code, bound to the endpoint it dialed and its own key,
/// so it can't be replayed to another endpoint or by another phone.
pub fn proof(secret: &[u8; 32], endpoint: &[u8; 32], device: &[u8; 32]) -> [u8; 32] {
    let mut msg = [0u8; 64];
    msg[..32].copy_from_slice(endpoint);
    msg[32..].copy_from_slice(device);
    *blake3::keyed_hash(secret, &msg).as_bytes()
}

/// Compares proofs in constant time.
pub fn proof_matches(expected: &[u8; 32], given: &[u8; 32]) -> bool {
    blake3::Hash::from_bytes(*expected) == blake3::Hash::from_bytes(*given)
}

/// A machine key as shown to people: its first 64 bits in four groups, like
/// [`key_groups`]. Shorter forms could be matched by a key made for the purpose.
pub fn fingerprint(key_hex: &str) -> String {
    key_groups(key_hex)
}

/// The first 64 bits of a key in four groups, such as `7f3a 9c1b 22de 81f0`. Shown on the
/// phone and in the approval prompt so the person can check they match.
pub fn key_groups(key_hex: &str) -> String {
    let head: Vec<&str> = (0..4)
        .filter_map(|i| key_hex.get(i * 4..i * 4 + 4))
        .collect();
    if head.len() == 4 {
        head.join(" ")
    } else {
        key_hex.to_string()
    }
}

/// What the QR code shown by `pgpu pair` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrPayload {
    /// The agent's public key in hex.
    pub agent_id: String,
    pub code: PairCode,
    /// Relay URLs the agent uses; empty means the default relays.
    pub relays: Vec<String>,
}

impl QrPayload {
    pub fn to_uri(&self) -> String {
        let mut s = format!(
            "{QR_PREFIX}v=1&id={}&code={}",
            self.agent_id,
            self.code.as_str()
        );
        for r in &self.relays {
            s.push_str("&relay=");
            s.push_str(&escape(r));
        }
        s
    }

    pub fn parse(uri: &str) -> Option<Self> {
        let query = uri.trim().strip_prefix(QR_PREFIX)?;
        let (mut id, mut code, mut relays) = (None, None, Vec::new());
        for part in query.split('&') {
            match part.split_once('=')? {
                ("v", v) if v != "1" => return None,
                ("id", v) if v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    id = Some(v.to_ascii_lowercase());
                }
                ("code", v) => code = PairCode::parse(v).ok(),
                ("relay", v) => relays.push(unescape(v)?),
                _ => {}
            }
        }
        Some(Self {
            agent_id: id?,
            code: code?,
            relays,
        })
    }
}

/// What the phone was given: a scanned QR code or a typed code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairInput {
    Qr(QrPayload),
    Code(PairCode),
}

impl PairInput {
    pub fn parse(input: &str) -> Result<Self, BadCode> {
        match QrPayload::parse(input) {
            Some(qr) => Ok(Self::Qr(qr)),
            None => PairCode::parse(input).map(Self::Code),
        }
    }

    pub fn code(&self) -> PairCode {
        match self {
            Self::Qr(qr) => qr.code,
            Self::Code(c) => *c,
        }
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b".-_~:/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn unescape(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_format_and_parse_leniently() {
        let code = PairCode::from_random([0x21, 0xE7, 0x91, 0x2B, 0xAD]);
        let shown = code.to_string();
        assert_eq!(shown.len(), 9);
        assert_eq!(&shown[4..5], "-");
        assert_eq!(PairCode::parse(&shown.to_lowercase()), Ok(code));
        assert_eq!(PairCode::parse(code.as_str()), Ok(code));
        assert_eq!(PairCode::parse("4f7k 92qd").unwrap().as_str(), "4F7K92QD");
        assert_eq!(PairCode::parse("O1LI-0000").unwrap().as_str(), "01110000");
        assert_eq!(PairCode::parse("4F7K-92Q"), Err(BadCode::Length));
        assert_eq!(PairCode::parse("4F7K-92QDX"), Err(BadCode::Length));
        assert_eq!(PairCode::parse("4F7K-92QU"), Err(BadCode::Char('U')));
    }

    #[test]
    fn derived_keys_differ_per_code_and_purpose() {
        let a = PairCode::parse("4F7K92QD").unwrap();
        let b = PairCode::parse("4F7K92QE").unwrap();
        assert_ne!(a.secret(), b.secret());
        assert_ne!(a.secret(), a.rendezvous_seed());
    }

    #[test]
    fn proof_binds_endpoint_and_device() {
        let s = PairCode::parse("4F7K92QD").unwrap().secret();
        let p = proof(&s, &[1; 32], &[2; 32]);
        assert!(proof_matches(&p, &proof(&s, &[1; 32], &[2; 32])));
        assert!(!proof_matches(&p, &proof(&s, &[3; 32], &[2; 32])));
        assert!(!proof_matches(&p, &proof(&s, &[1; 32], &[3; 32])));
    }

    #[test]
    fn qr_payload_roundtrips() {
        let qr = QrPayload {
            agent_id: "ab".repeat(32),
            code: PairCode::parse("4F7K92QD").unwrap(),
            relays: vec![
                "https://relay.example.com./".into(),
                "https://x/?a=b&c".into(),
            ],
        };
        let uri = qr.to_uri();
        assert_eq!(QrPayload::parse(&uri), Some(qr.clone()));
        assert_eq!(PairInput::parse(&uri), Ok(PairInput::Qr(qr)));
        assert!(matches!(
            PairInput::parse("4f7k-92qd"),
            Ok(PairInput::Code(_))
        ));
        assert_eq!(QrPayload::parse("pocketgpu://pair?v=2&id=x"), None);
    }

    #[test]
    fn fingerprints_show_64_bits() {
        assert_eq!(fingerprint(&"a91c40de".repeat(8)), "a91c 40de a91c 40de");
        assert_eq!(fingerprint("abc"), "abc");
        assert_eq!(
            key_groups(&"7f3a9c1b22de81f0".repeat(4)),
            "7f3a 9c1b 22de 81f0"
        );
    }
}
