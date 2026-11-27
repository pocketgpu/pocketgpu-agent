//! The audit's summary of an assistant's action (F32): secret-looking parts replaced by
//! `•••`, cut to the audit's 200 characters. The full command never reaches the disk.

/// What a hidden part shows.
pub const MASK: &str = "•••";
/// Characters kept, the audit's own limit.
pub const SUMMARY_MAX: usize = 200;

/// Options whose value is a secret, written `--name=value` or `--name value`.
const SECRET_OPTIONS: [&str; 9] = [
    "password", "passwd", "pass", "token", "secret", "api-key", "apikey", "api_key", "auth",
];
/// `name=value` anywhere in a word, such as a URL's query or an environment variable.
const SECRET_KEYS: [&str; 8] = [
    "token=",
    "password=",
    "passwd=",
    "secret=",
    "api_key=",
    "apikey=",
    "access_key=",
    "pwd=",
];
/// Prefixes of well-known keys, masked to the end of the word.
const KEY_PREFIXES: [&str; 7] = ["ghp_", "gho_", "ghs_", "ghu_", "ghr_", "github_pat_", "sk-"];
/// `find`'s options that start like `-p…` but aren't passwords.
const NOT_PASSWORDS: [&str; 5] = ["-print", "-print0", "-prune", "-path", "-perm"];

/// Masked and cut: what the audit keeps of a command.
pub fn summary(text: &str) -> String {
    let masked = mask(text);
    let flat: String = masked.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SUMMARY_MAX {
        return flat;
    }
    let mut out: String = flat.chars().take(SUMMARY_MAX - 1).collect();
    out.push('…');
    out
}

/// Secret-looking parts replaced by [`MASK`]; spacing kept.
pub fn mask(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut hide_next = false;
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String, hide_next: &mut bool| {
        if word.is_empty() {
            return;
        }
        if std::mem::take(hide_next) {
            out.push_str(MASK);
        } else {
            let (masked, next) = mask_word(word);
            out.push_str(&masked);
            *hide_next = next;
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_whitespace() {
            flush(&mut word, &mut out, &mut hide_next);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out, &mut hide_next);
    out
}

/// One word masked, and whether the next word is a secret (`Bearer x`, `--token x`).
fn mask_word(word: &str) -> (String, bool) {
    let lower = word.to_lowercase();
    let bare = lower.trim_matches(|c| c == '"' || c == '\'');
    if bare == "bearer" || bare.ends_with(":bearer") {
        return (word.to_string(), true);
    }
    if let Some(name) = bare.strip_prefix("--") {
        let (name, value) = match name.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (name, None),
        };
        if SECRET_OPTIONS
            .iter()
            .any(|s| name == *s || name.ends_with(&format!("-{s}")))
        {
            return match value {
                Some(_) => {
                    let at = word.find('=').unwrap_or(word.len());
                    (format!("{}={MASK}", &word[..at]), false)
                }
                None => (word.to_string(), true),
            };
        }
    }
    if word.len() > 2 && word.starts_with("-p") && !NOT_PASSWORDS.contains(&word) {
        return (format!("-p{MASK}"), false);
    }
    let mut w = word.to_string();
    w = mask_keys(&w);
    w = mask_url_user(&w);
    w = mask_prefixed(&w);
    (w, false)
}

/// `token=abc&x=1` → `token=•••&x=1`.
fn mask_keys(word: &str) -> String {
    // ASCII only, so its byte offsets are the word's.
    let lower = word.to_ascii_lowercase();
    for key in SECRET_KEYS {
        let mut search = 0;
        while let Some(found) = lower[search..].find(key) {
            let at = search + found;
            let starts_word = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
            let value_at = at + key.len();
            if starts_word && !word[value_at..].starts_with(MASK) {
                let end = word[value_at..]
                    .find(['&', ';', ',', '"', '\''])
                    .map_or(word.len(), |e| value_at + e);
                if end > value_at {
                    let out = format!("{}{MASK}{}", &word[..value_at], &word[end..]);
                    return mask_keys(&out);
                }
            }
            search = value_at;
        }
    }
    word.to_string()
}

/// `https://me:pw@host/x` → `https://•••@host/x`.
fn mask_url_user(word: &str) -> String {
    let Some(scheme) = word.find("://") else {
        return word.to_string();
    };
    let rest_at = scheme + 3;
    let rest = &word[rest_at..];
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..host_end].rfind('@') {
        Some(at) if rest[..at].contains(':') && !rest[..at].starts_with(MASK) => {
            format!("{}{MASK}{}", &word[..rest_at], &rest[at..])
        }
        _ => word.to_string(),
    }
}

/// `ghp_…`, `sk-…` and `AKIA…` keys, from where they start to the end of the key.
fn mask_prefixed(word: &str) -> String {
    let bytes = word.as_bytes();
    let key_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    let mut out = String::new();
    let mut i = 0;
    while i < word.len() {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let rest = &word[i..];
        let len = if !boundary {
            0
        } else if let Some(p) = KEY_PREFIXES.iter().find(|p| rest.starts_with(**p)) {
            let n = rest.bytes().take_while(|b| key_char(*b)).count();
            if n >= p.len() + 8 { n } else { 0 }
        } else if rest.starts_with("AKIA") || rest.starts_with("ASIA") {
            let n = rest
                .bytes()
                .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                .count();
            if n >= 16 { n } else { 0 }
        } else {
            0
        };
        if len > 0 {
            out.push_str(MASK);
            i += len;
        } else {
            let c = rest.chars().next().expect("not at the end");
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_masked() {
        let table = [
            ("mysql --password=hunter2 db", "mysql --password=••• db"),
            ("app --token abc123 run", "app --token ••• run"),
            ("app --db-password=x", "app --db-password=•••"),
            ("mysql -u root -phunter2 db", "mysql -u root -p••• db"),
            (
                "curl 'https://x.io/a?token=abc&b=1'",
                "curl 'https://x.io/a?token=•••&b=1'",
            ),
            ("API_TOKEN=abc ./run", "API_TOKEN=••• ./run"),
            ("export GITHUB_TOKEN=x", "export GITHUB_TOKEN=•••"),
            ("a?token=x&password=y", "a?token=•••&password=•••"),
            ("PASSWORD=pw ./run", "PASSWORD=••• ./run"),
            (
                "curl -H 'Authorization: Bearer eyJhbGciOi' x",
                "curl -H 'Authorization: Bearer ••• x",
            ),
            // AWS's documented example key, in two halves so the public export's secret scan passes.
            (
                concat!("aws s3 ls AKIA", "IOSFODNN7EXAMPLE"),
                "aws s3 ls •••",
            ),
            (
                "gh auth login ghp_abcdefghijklmnop1234",
                "gh auth login •••",
            ),
            (
                "OPENAI_API_KEY=sk-proj-abcdefgh1234 x",
                "OPENAI_API_KEY=••• x",
            ),
            ("echo sk-abcdefghijk", "echo •••"),
            (
                "git clone https://me:pw@github.com/a/b",
                "git clone https://•••@github.com/a/b",
            ),
            (
                "git clone https://github.com/a/b",
                "git clone https://github.com/a/b",
            ),
            ("df --disk-usage task-runner", "df --disk-usage task-runner"),
            ("find . -print -path x", "find . -print -path x"),
            ("rm -rf build dist .cache", "rm -rf build dist .cache"),
        ];
        for (text, want) in table {
            assert_eq!(mask(text), want, "{text:?}");
        }
    }

    #[test]
    fn a_key_after_a_secret_name_is_masked_once() {
        assert_eq!(mask("API_TOKEN=ghp_abcdefghijklmnop"), "API_TOKEN=•••");
    }

    #[test]
    fn summaries_are_one_line_and_cut() {
        assert_eq!(summary("rm -rf\n  build"), "rm -rf build");
        let long = summary(&format!("echo {}", "x".repeat(500)));
        assert_eq!(long.chars().count(), SUMMARY_MAX);
        assert!(long.ends_with('…'));
        // Masked before the cut, so a cut never leaves half a key.
        let key = format!("{} ghp_abcdefghijklmnop", "a".repeat(190));
        assert!(!summary(&key).contains("ghp_"));
    }
}
