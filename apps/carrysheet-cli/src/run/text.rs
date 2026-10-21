//! Showing text that comes from a file: names, paths inside packages,
//! error details and macro source. Such text may hold escape sequences,
//! direction overrides, invisible characters or forged lines written to
//! mislead a person or an AI agent, so it never reaches output raw.

/// The most characters of a name from the file that are shown. Real VBA
/// names have at most 31.
pub const MAX_NAME_CHARS: usize = 64;

/// Characters that change how text looks without being seen: direction
/// marks and overrides, zero-width characters, tag characters, the byte
/// order mark and the Unicode line and paragraph separators.
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// True when the text holds control characters (other than tab and line
/// breaks) or invisible ones.
pub fn has_hidden(text: &str) -> bool {
    text.chars()
        .any(|c| is_invisible(c) || (c.is_control() && !matches!(c, '\t' | '\n' | '\r')))
}

/// Text for a terminal: every control character but tab and line feed,
/// and every invisible character, is shown as `�`.
pub fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            if is_invisible(c) || (c.is_control() && !matches!(c, '\t' | '\n')) {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// A string from the file, made safe to show anywhere: control and
/// invisible characters written as `\u{…}`, and cut to `max` characters
/// with a marker that says how much was left out.
pub fn quoted(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    for c in chars.by_ref().take(max) {
        if is_invisible(c) || c.is_control() {
            out.push_str(&format!("\\u{{{:04X}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    let rest = chars.count();
    if rest > 0 {
        out.push_str(&format!("…[cut: {rest} more characters]"));
    }
    out
}

/// A name from the file, quoted and cut to [`MAX_NAME_CHARS`].
pub fn name(text: &str) -> String {
    quoted(text, MAX_NAME_CHARS)
}

/// True for a name the VBA editor accepts for a module: a letter first,
/// then letters, digits or `_`, at most 31 characters.
pub fn is_vba_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(char::is_alphabetic)
        && chars.all(|c| c.is_alphanumeric() || c == '_')
        && name.chars().count() <= 31
}

/// True when a source line is an `Attribute` line, which the VBA editor
/// hides. Matched like the risk scanner: any case, after leading spaces,
/// as a whole word.
pub fn is_attribute_line(line: &str) -> bool {
    let t = line.trim_start().as_bytes();
    t.len() >= 9
        && t[..9].eq_ignore_ascii_case(b"attribute")
        && t.get(9)
            .is_none_or(|&b| !(b.is_ascii_alphanumeric() || b == b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_hides_controls_and_invisible_characters() {
        assert_eq!(
            plain("a\x1b[31mb\u{202E}c\td\u{200B}e\u{E0041}f\u{FEFF}\u{2028}\r"),
            "a\u{FFFD}[31mb\u{FFFD}c\td\u{FFFD}e\u{FFFD}f\u{FFFD}\u{FFFD}\u{FFFD}"
        );
        assert_eq!(plain("line\nnext"), "line\nnext");
    }

    #[test]
    fn quoted_escapes_and_cuts() {
        assert_eq!(quoted("a\x1bb\u{202E}", 10), "a\\u{001B}b\\u{202E}");
        assert_eq!(quoted("a\nb", 10), "a\\u{000A}b");
        assert_eq!(quoted("abcdef", 3), "abc…[cut: 3 more characters]");
        assert_eq!(quoted("abc", 3), "abc");
        assert_eq!(quoted("", 3), "");
    }

    #[test]
    fn vba_identifiers() {
        assert!(is_vba_identifier("Module1"));
        assert!(is_vba_identifier("Módulo1"));
        assert!(!is_vba_identifier(" Module1"));
        assert!(!is_vba_identifier("1Module"));
        assert!(!is_vba_identifier("Mod\u{200B}ule"));
        assert!(!is_vba_identifier(&"a".repeat(32)));
        assert!(!is_vba_identifier(""));
    }

    #[test]
    fn attribute_lines_like_the_scanner() {
        assert!(is_attribute_line("Attribute VB_Name = \"M\""));
        assert!(is_attribute_line("   attribute x.VB_UserMemId = 0"));
        assert!(is_attribute_line("ATTRIBUTE"));
        assert!(!is_attribute_line("Attributes = 1"));
        assert!(!is_attribute_line("' Attribute"));
    }
}
