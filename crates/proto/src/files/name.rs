//! The name a sent file is saved under (FR-004), shared by the agent and the apps.

use super::MAX_NAME;

/// Characters Unicode marks as invisible formatting, direction overrides among them: with
/// U+202E, `photo` + U+202E + `gpj.exe` shows as `photoexe.jpg`. Joiners (U+200C, U+200D)
/// stay, since some scripts and emoji need them.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}'
        | '\u{0600}'..='\u{0605}'
        | '\u{061C}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{180E}'
        | '\u{200B}'
        | '\u{200E}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{E0001}'
        | '\u{E0020}'..='\u{E007F}')
}

/// Names Windows keeps for devices, with or without an extension (`nul.txt` too).
fn reserved_on_windows(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = stem.to_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let mut chars = upper.chars();
            let head: String = chars.by_ref().take(3).collect();
            let tail: Vec<char> = chars.collect();
            (head == "COM" || head == "LPT")
                && matches!(tail.as_slice(), [d] if d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
        }
    }
}

/// The name the agent saves a sent file under (FR-004), the same on every system: only the
/// last part of a path; no control or invisible formatting characters; `<>:"|?*` become
/// `_`; no trailing dots or spaces; no leading dot (hidden); no Windows device names; at
/// most [`MAX_NAME`] bytes, keeping the extension. Never empty.
pub fn clean_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = last
        .chars()
        .filter(|c| !c.is_control() && !invisible(*c))
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c => c,
        })
        .collect();
    name.truncate(name.trim_end_matches(['.', ' ']).len());
    if name.len() > MAX_NAME {
        name = shorten(&name);
    }
    if name.starts_with('.') {
        name.replace_range(..1, "_");
    }
    if reserved_on_windows(&name) {
        name.insert(0, '_');
        if name.len() > MAX_NAME {
            name = shorten(&name);
        }
    }
    if name.is_empty() {
        return "file".into();
    }
    name
}

/// Cuts `name` to [`MAX_NAME`] bytes on a character boundary, keeping a short extension,
/// and drops the dots and spaces the cut leaves at the end.
fn shorten(name: &str) -> String {
    fn cut(s: &str, max: usize) -> &str {
        let mut end = max.min(s.len());
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].trim_end_matches(['.', ' '])
    }
    let ext = match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 32 => &name[i..],
        _ => "",
    };
    let stem = cut(&name[..name.len() - ext.len()], MAX_NAME - ext.len());
    if stem.is_empty() {
        return cut(name, MAX_NAME).to_string();
    }
    format!("{stem}{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_ordinary_names() {
        for name in [
            "photo.jpg",
            "adapter.safetensors",
            "Résumé 2026.pdf",
            "日本語のファイル.txt",
            "a b c",
            "v1.2.tar.gz",
            "👨‍👩‍👧 family.png",
            "COM10",
            "console.log",
            "LPTX",
        ] {
            assert_eq!(clean_name(name), name);
        }
    }

    #[test]
    fn keeps_only_the_last_part_of_a_path() {
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("/home/me/.ssh/id_ed25519"), "id_ed25519");
        assert_eq!(clean_name(r"C:\Users\me\photo.jpg"), "photo.jpg");
        assert_eq!(clean_name(r"..\..\Windows\win.ini"), "win.ini");
        assert_eq!(clean_name("dir/"), "file");
        assert_eq!(clean_name(".."), "file");
        assert_eq!(clean_name("."), "file");
        assert_eq!(clean_name(""), "file");
    }

    #[test]
    fn drops_control_and_direction_characters() {
        assert_eq!(clean_name("a\u{0}b\nc\td\u{1b}[2K.txt"), "abcd[2K.txt");
        assert_eq!(clean_name("photo\u{202E}gpj.exe"), "photogpj.exe");
        for c in [
            '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{2066}',
            '\u{2067}', '\u{2068}', '\u{2069}', '\u{061C}', '\u{FEFF}', '\u{200B}', '\u{00AD}',
        ] {
            assert_eq!(clean_name(&format!("a{c}b.txt")), "ab.txt", "{:?}", c);
        }
        assert_eq!(clean_name("\u{202E}\u{200F}"), "file");
    }

    #[test]
    fn replaces_characters_windows_refuses() {
        assert_eq!(clean_name(r#"a<b>c:d"e|f?g*h.txt"#), "a_b_c_d_e_f_g_h.txt");
        // An alternate data stream on NTFS.
        assert_eq!(clean_name("x.txt:Zone.Identifier"), "x.txt_Zone.Identifier");
    }

    #[test]
    fn no_trailing_dots_or_spaces_and_nothing_hidden() {
        assert_eq!(clean_name("report.pdf. . "), "report.pdf");
        assert_eq!(clean_name("name   "), "name");
        assert_eq!(clean_name(".bashrc"), "_bashrc");
        assert_eq!(clean_name("..hidden"), "_.hidden");
        assert_eq!(clean_name(".hidden. "), "_hidden");
        assert_eq!(clean_name(" lead"), " lead");
    }

    #[test]
    fn windows_device_names_get_a_prefix() {
        for name in [
            "CON",
            "con",
            "Prn",
            "aux",
            "NUL",
            "nul.txt",
            "COM1",
            "com9.log",
            "LPT1",
            "lpt3.tar.gz",
            "COM0",
            "CONIN$",
            "conout$.x",
            "COM¹",
            "nul .txt",
            "CON.",
        ] {
            let clean = clean_name(name);
            assert!(clean.starts_with('_'), "{name} → {clean}");
        }
        assert_eq!(clean_name("aux.c"), "_aux.c");
        assert_eq!(clean_name("CON."), "_CON");
    }

    #[test]
    fn long_names_keep_their_extension() {
        let long = format!("{}.safetensors", "a".repeat(400));
        let clean = clean_name(&long);
        assert_eq!(clean.len(), MAX_NAME);
        assert!(clean.ends_with(".safetensors"));

        // Never cut inside a character.
        let wide = format!("{}.png", "é".repeat(300));
        let clean = clean_name(&wide);
        assert!(
            clean.len() <= MAX_NAME && clean.ends_with(".png"),
            "{clean}"
        );
        assert!(clean.trim_end_matches(".png").chars().all(|c| c == 'é'));

        // An "extension" too long to be one is cut like the rest.
        let odd = format!("a.{}", "b".repeat(300));
        let clean = clean_name(&odd);
        assert_eq!(clean.len(), MAX_NAME);
        assert!(clean.starts_with("a.b"));

        // A cut that ends in dots or spaces loses them.
        let spaced = format!("{}   {}", "x".repeat(198), "y".repeat(10));
        assert_eq!(clean_name(&spaced), "x".repeat(198));
        // Never only an extension, which would be a hidden file.
        assert_eq!(clean_name(&format!("{}.txt", " ".repeat(250))), "file");
        // A device name cut to length keeps its prefix.
        let device = format!("CON.{}", "x".repeat(300));
        let clean = clean_name(&device);
        assert!(
            clean.starts_with("_CON.") && clean.len() == MAX_NAME,
            "{clean}"
        );
    }

    #[test]
    fn cleaning_twice_changes_nothing() {
        for raw in [
            "../a.txt",
            ".x",
            "CON",
            "a\u{202E}b",
            "x:y",
            "dots...",
            &"z".repeat(500),
        ] {
            let once = clean_name(raw);
            assert_eq!(clean_name(&once), once, "{raw}");
        }
    }
}
