//! Reads the top-level string fields from the start of a JSON object that was cut short.
//! A hook's input can be larger than `pgpu hook` reads (a pasted log in the prompt, a long
//! last answer); the few short fields it needs usually come before the long ones.

use std::collections::HashMap;

use super::hook::FIELDS;

/// The [`FIELDS`] found as top-level strings before the input ends or stops making sense.
pub fn top_level_strings(input: &str) -> HashMap<&'static str, String> {
    let b = input.as_bytes();
    let mut out = HashMap::new();
    let mut i = skip_ws(b, 0);
    if b.get(i) != Some(&b'{') {
        return out;
    }
    i += 1;
    loop {
        i = skip_ws(b, i);
        if b.get(i) == Some(&b',') {
            i = skip_ws(b, i + 1);
        }
        let Some((key, next)) = string(b, i) else {
            return out;
        };
        i = skip_ws(b, next);
        if b.get(i) != Some(&b':') {
            return out;
        }
        i = skip_ws(b, i + 1);
        if b.get(i) == Some(&b'"') {
            let Some((value, next)) = string(b, i) else {
                return out;
            };
            if let Some(k) = FIELDS.iter().find(|k| **k == key) {
                out.entry(*k).or_insert(value);
            }
            i = next;
        } else {
            let Some(next) = skip_value(b, i) else {
                return out;
            };
            i = next;
        }
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// The string starting at `i` (decoded) and where it ends; `None` if it is cut.
fn string(b: &[u8], i: usize) -> Option<(String, usize)> {
    let end = string_end(b, i)?;
    let s = serde_json::from_slice::<String>(&b[i..end]).ok()?;
    Some((s, end))
}

/// Just past the closing quote of the string starting at `i`.
fn string_end(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'"') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

/// Past a number, `true`, `false`, `null`, object or array starting at `i`, stopping before
/// the `,` or `}` that follows it; `None` if it is cut.
fn skip_value(b: &[u8], mut i: usize) -> Option<usize> {
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i = string_end(b, i)?;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' if depth == 0 => return Some(i),
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b',' if depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_before_the_cut() {
        let cut = r#"{"session_id":"abc","cwd":"/x","n":3,"ok":true,"list":[1,{"a":"}"}],
            "hook_event_name":"UserPromptSubmit","prompt":"a very long pas"#;
        let f = top_level_strings(cut);
        assert_eq!(f.get("session_id").map(String::as_str), Some("abc"));
        assert_eq!(
            f.get("hook_event_name").map(String::as_str),
            Some("UserPromptSubmit")
        );
        assert_eq!(f.len(), 2, "only the fields it reads: {f:?}");
    }

    #[test]
    fn fields_inside_values_are_not_top_level() {
        let tricky = r#"{"prompt":"say \"hook_event_name\":\"Stop\"","inner":{"hook_event_name":"Stop"},"session_id":"s"#;
        let f = top_level_strings(tricky);
        assert_eq!(f.get("hook_event_name"), None);
        assert_eq!(f.get("session_id"), None, "cut inside the value");
        let escaped = r#"{"session_id":"a\"bA","#;
        assert_eq!(
            top_level_strings(escaped)
                .get("session_id")
                .map(String::as_str),
            Some("a\"bA")
        );
    }

    #[test]
    fn nonsense_gives_nothing() {
        for s in ["", "[", "nope", "{", "{\"a\"", "{\"a\":", "{1:2}", "\"x\""] {
            assert!(top_level_strings(s).is_empty(), "{s:?}");
        }
    }
}
