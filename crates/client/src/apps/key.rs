//! The key to a phone's forward (F42). On a phone every app shares `127.0.0.1`, so a
//! forward there lets in only the viewer that opened it. The viewer's first address carries
//! the forward's key once (`/__pgpu/<key>/<path>`); the answer keeps the key as a cookie
//! (`HttpOnly`, `SameSite=Strict`) and sends the viewer on to `<path>`. Every later
//! connection's first request, WebSocket ones too, must carry that cookie. Anything else is
//! turned away here, before it reaches the machine.

use std::fmt;

/// Where a viewer goes in first: `/__pgpu/<key>` and then the path to start at.
const ENTRY: &str = "/__pgpu/";
/// The start of every forward's cookie name; the port follows.
const COOKIE: &str = "pgpu_fwd_";

/// A forward's random key, and the name of the cookie that carries it.
pub(super) struct Key {
    secret: String,
    cookie: String,
}

/// What to do with a connection, from its first request.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// It carries the key: on to the machine.
    Pass,
    /// Answer this (the entry's cookie and where to go, or a refusal) and close.
    Answer(String),
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}

impl Key {
    /// A new key for the forward at `127.0.0.1:<local>`: 128 random bits.
    pub(super) fn new(local: u16) -> Self {
        let bytes = iroh::SecretKey::generate().to_bytes();
        let secret = bytes[..16].iter().map(|b| format!("{b:02x}")).collect();
        Self::with(secret, local)
    }

    fn with(secret: String, local: u16) -> Self {
        // Cookies don't tell ports apart: each forward has its own name. The phone's viewer
        // loads nothing from another port here, so no other app there is sent it.
        Self {
            secret,
            cookie: format!("{COOKIE}{local}"),
        }
    }

    /// The path a viewer opens first to start at `path` (`/`, Jupyter's `/?token=…`).
    pub(super) fn entry(&self, path: &str) -> String {
        format!("{ENTRY}{}{}", self.secret, start(path))
    }

    /// Whether the connection whose first request is `head` gets in.
    pub(super) fn check(&self, head: &[u8]) -> Verdict {
        let Ok(text) = std::str::from_utf8(head) else {
            return Verdict::Answer(refused());
        };
        let mut lines = text.split("\r\n");
        let target = lines.next().and_then(|l| l.split(' ').nth(1)).unwrap_or("");
        if let Some(rest) = target.strip_prefix(ENTRY) {
            let at = rest.find(['/', '?']).unwrap_or(rest.len());
            let (given, path) = rest.split_at(at);
            if !same(given, &self.secret) {
                return Verdict::Answer(refused());
            }
            let path = if path.starts_with('?') {
                format!("/{path}")
            } else {
                path.to_string()
            };
            return Verdict::Answer(self.enter(start(&path)));
        }
        let carried = lines
            .take_while(|l| !l.is_empty())
            .filter_map(|l| l.split_once(':'))
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("cookie"))
            .flat_map(|(_, value)| value.split(';'))
            .filter_map(|c| c.split_once('='))
            .any(|(name, value)| name.trim() == self.cookie && same(value.trim(), &self.secret));
        if carried {
            Verdict::Pass
        } else {
            Verdict::Answer(refused())
        }
    }

    /// The entry's answer: keep the key, then go to `path`.
    fn enter(&self, path: &str) -> String {
        format!(
            "HTTP/1.1 303 See Other\r\nlocation: {path}\r\n\
             set-cookie: {}={}; Path=/; HttpOnly; SameSite=Strict\r\n\
             cache-control: no-store\r\nconnection: close\r\ncontent-length: 0\r\n\r\n",
            self.cookie, self.secret
        )
    }
}

/// `head` (a connection's first request) without any forward's key cookie, so the apps on
/// the machine never see one: the viewer sends every forward's cookie to each of them. A
/// `Cookie` line left empty goes too. Later requests on the same connection go as they are.
pub(super) fn without_keys(head: Vec<u8>) -> Vec<u8> {
    let end = super::head_end(&head);
    let Ok(text) = std::str::from_utf8(&head[..end]) else {
        return head;
    };
    if !text.contains(COOKIE) {
        return head;
    }
    let mut out = String::with_capacity(head.len());
    for (i, line) in text.split("\r\n").enumerate() {
        let cookies = (i > 0)
            .then(|| line.split_once(':'))
            .flatten()
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("cookie"));
        let kept;
        let line = match cookies {
            Some((name, value)) => {
                let rest: Vec<&str> = value
                    .split(';')
                    .map(str::trim)
                    .filter(|c| !c.is_empty() && !c.starts_with(COOKIE))
                    .collect();
                if rest.is_empty() {
                    continue;
                }
                kept = format!("{name}: {}", rest.join("; "));
                kept.as_str()
            }
            None => line,
        };
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(line);
    }
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&head[end..]);
    bytes
}

/// `path` when it is a plain path on this address, else `/`: never another site
/// (`//host`), and nothing that could end the header it goes in.
fn start(path: &str) -> &str {
    let plain = path.starts_with('/')
        && !path.starts_with("//")
        && !path.starts_with("/\\")
        && path.bytes().all(|b| b > b' ' && b != 0x7f);
    if plain { path } else { "/" }
}

/// Equal, in a time that doesn't depend on where they first differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// The answer to a connection without the key.
fn refused() -> String {
    let body = "Open this app from PocketGPU.\n";
    format!(
        "HTTP/1.1 403 Forbidden\r\ncontent-type: text/plain\r\nconnection: close\r\n\
         content-length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        Key::with("00112233445566778899aabbccddeeff".into(), 41237)
    }

    fn req(target: &str, extra: &str) -> Vec<u8> {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:41237\r\n{extra}\r\n").into_bytes()
    }

    fn location(v: Verdict) -> String {
        let Verdict::Answer(a) = v else {
            panic!("expected an answer, got {v:?}")
        };
        assert!(a.starts_with("HTTP/1.1 303"), "{a}");
        assert!(
            a.contains("set-cookie: pgpu_fwd_41237=00112233445566778899aabbccddeeff; Path=/; HttpOnly; SameSite=Strict\r\n"),
            "{a}"
        );
        let at = a.find("location: ").unwrap() + "location: ".len();
        a[at..a[at..].find("\r\n").unwrap() + at].to_string()
    }

    fn is_refused(v: Verdict) -> bool {
        matches!(v, Verdict::Answer(a) if a.starts_with("HTTP/1.1 403"))
    }

    #[test]
    fn another_app_on_the_phone_is_turned_away() {
        let k = key();
        // What a port scan, or any other program here, sends: no cookie.
        assert!(is_refused(k.check(&req("/", ""))));
        assert!(is_refused(k.check(&req("/prompt", "Cookie: a=b\r\n"))));
        // A wrong key, a wrong cookie, the key under another forward's name.
        assert!(is_refused(
            k.check(&req("/__pgpu/00112233445566778899aabbccddeef0/", ""))
        ));
        assert!(is_refused(k.check(&req("/__pgpu//", ""))));
        assert!(is_refused(
            k.check(&req("/", "Cookie: pgpu_fwd_41237=nope\r\n"))
        ));
        assert!(is_refused(k.check(&req(
            "/",
            "Cookie: pgpu_fwd_9999=00112233445566778899aabbccddeeff\r\n"
        ))));
        assert!(is_refused(k.check(b"\xff\xfe\r\n\r\n")));
    }

    #[test]
    fn the_viewer_enters_once_then_carries_the_cookie() {
        let k = key();
        let entry = k.entry("/?token=f00d");
        assert_eq!(
            entry,
            "/__pgpu/00112233445566778899aabbccddeeff/?token=f00d"
        );
        assert_eq!(location(k.check(&req(&entry, ""))), "/?token=f00d");
        assert_eq!(location(k.check(&req(&k.entry("/"), ""))), "/");
        assert_eq!(
            location(k.check(&req("/__pgpu/00112233445566778899aabbccddeeff", ""))),
            "/"
        );
        assert_eq!(
            location(k.check(&req("/__pgpu/00112233445566778899aabbccddeeff?x=1", ""))),
            "/?x=1"
        );
        // Then every request, a WebSocket's too, with the cookie among others.
        let cookie = "Cookie: _xsrf=1; pgpu_fwd_41237=00112233445566778899aabbccddeeff\r\n";
        assert_eq!(k.check(&req("/lab", cookie)), Verdict::Pass);
        let ws = format!("{cookie}Upgrade: websocket\r\nConnection: Upgrade\r\n");
        assert_eq!(k.check(&req("/api/kernels/1/channels", &ws)), Verdict::Pass);
    }

    #[test]
    fn the_entry_goes_nowhere_else() {
        let k = key();
        for path in ["//evil.example/", "/\\evil.example", "/a\tb"] {
            let target = format!("/__pgpu/00112233445566778899aabbccddeeff{path}");
            assert_eq!(location(k.check(&req(&target, ""))), "/", "{path}");
        }
        assert_eq!(
            k.entry("//evil.example"),
            "/__pgpu/00112233445566778899aabbccddeeff/"
        );
    }

    #[test]
    fn the_machine_never_sees_a_key() {
        let head = |cookie: &str| {
            format!("POST /prompt HTTP/1.1\r\nHost: 127.0.0.1:41237\r\n{cookie}Accept: */*\r\n\r\n")
                .into_bytes()
        };
        let body = b"\x89PNG\xff";
        let with_body = |h: Vec<u8>| [h, body.to_vec()].concat();
        // Every forward's key goes, the app's own cookies stay, and the body after the head.
        let sent = head("Cookie: _xsrf=1; pgpu_fwd_41237=aa;pgpu_fwd_9=bb; theme=dark\r\n");
        assert_eq!(
            without_keys(with_body(sent)),
            with_body(head("Cookie: _xsrf=1; theme=dark\r\n"))
        );
        // A line with only keys goes; one header per line, any case.
        let sent = head("cookie: pgpu_fwd_41237=aa\r\nCOOKIE: a=b; pgpu_fwd_1=c\r\n");
        assert_eq!(without_keys(sent), head("COOKIE: a=b\r\n"));
        // Nothing to take out: as it came.
        let plain = with_body(head("Cookie: a=b\r\n"));
        assert_eq!(without_keys(plain.clone()), plain);
        assert_eq!(without_keys(head("")), head(""));
    }

    #[test]
    fn each_forward_has_its_own_key_and_hides_it() {
        let (a, b) = (Key::new(1), Key::new(1));
        assert_eq!(a.secret.len(), 32);
        assert_ne!(a.secret, b.secret);
        assert_eq!(format!("{a:?}"), "Key(..)");
    }
}
