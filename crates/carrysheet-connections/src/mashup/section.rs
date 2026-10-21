//! Splitting a Power Query M section document (`Formulas/Section1.m`) into
//! its members, following the section grammar in Microsoft's Power Query M
//! language specification:
//!
//! ```text
//! section Section1;
//! shared Query1 = let Source = ... in Source;
//! [ Description = "..." ] shared #"My Query" = ...;
//! ```
//!
//! Members end with a `;` outside strings, quoted identifiers and comments;
//! inside an expression M separates with `,`, so this split is exact without
//! parsing the expressions.

/// One member: its name (quoted identifiers unquoted) and its formula.
pub(crate) struct Member {
    pub name: String,
    pub formula: String,
}

/// Lists up to `max_members` members; the flag says whether more were left.
pub(crate) fn members(text: &str, max_members: usize) -> (Vec<Member>, bool) {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut out = Vec::new();
    let mut more = false;
    statements(text, |statement| {
        let Some(member) = member(statement) else {
            return true;
        };
        if out.len() >= max_members {
            more = true;
            return false;
        }
        out.push(member);
        true
    });
    (out, more)
}

/// Splits on `;` that are outside strings, quoted identifiers and comments,
/// handing each statement to `visit` until it returns false. Nothing is
/// collected, so a text of many `;` costs time, not memory.
fn statements<'t>(text: &'t str, mut visit: impl FnMut(&'t str) -> bool) {
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = skip_string(bytes, i + 1),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = bytes[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(bytes.len(), |p| i + p);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |p| i + 2 + p + 2);
            }
            b';' => {
                if !visit(&text[start..i]) {
                    return;
                }
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if !text[start..].trim().is_empty() {
        visit(&text[start..]);
    }
}

/// Position after the `"` that closes a string starting at `from` (`""` is
/// an escaped quote).
fn skip_string(bytes: &[u8], from: usize) -> usize {
    let mut k = from;
    while k < bytes.len() {
        if bytes[k] == b'"' {
            if bytes.get(k + 1) == Some(&b'"') {
                k += 2;
                continue;
            }
            return k + 1;
        }
        k += 1;
    }
    bytes.len()
}

/// Skips spaces and comments.
fn skip_trivia(text: &str) -> &str {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if let Some(after) = trimmed.strip_prefix("//") {
            rest = after.find('\n').map_or("", |p| &after[p..]);
        } else if let Some(after) = trimmed.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |p| &after[p + 2..]);
        } else {
            return trimmed;
        }
    }
}

fn member(statement: &str) -> Option<Member> {
    let mut rest = skip_trivia(statement);
    if starts_with_keyword(rest, "section") {
        return None;
    }
    if rest.starts_with('[') {
        rest = skip_trivia(skip_attributes(rest)?);
    }
    if starts_with_keyword(rest, "shared") {
        rest = skip_trivia(&rest["shared".len()..]);
    }
    let (name, after) = if let Some(quoted) = rest.strip_prefix("#\"") {
        let end = skip_string(quoted.as_bytes(), 0);
        let inner = quoted.get(..end.checked_sub(1)?)?;
        (inner.replace("\"\"", "\""), &quoted[end..])
    } else {
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        (rest[..end].to_string(), &rest[end..])
    };
    let formula = skip_trivia(after).strip_prefix('=')?;
    Some(Member {
        name,
        formula: formula.trim().to_string(),
    })
}

fn starts_with_keyword(text: &str, keyword: &str) -> bool {
    text.starts_with(keyword)
        && !text[keyword.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Skips a `[ ... ]` attribute record, minding nested brackets and strings.
fn skip_attributes(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = skip_string(bytes, i + 1);
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&text[i + 1..]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::members;

    #[test]
    fn splits_members() {
        let text = "\u{FEFF}section Section1;\r\n\r\nshared Sales = let\r\n    Source = Sql.Database(\"s;1\", \"db\")\r\nin\r\n    Source;\r\n\r\n// a comment; with a semicolon\r\n[ Description = \"x;y\" ] shared #\"My \"\"Query\"\"\" = 1 /* ; */ + 2;\r\nshared   Plain=\"a\"\"b;\";";
        let (list, more) = members(text, 10);
        assert!(!more);
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Sales", "My \"Query\"", "Plain"]);
        assert!(list[0].formula.starts_with("let"));
        assert!(list[0].formula.ends_with("Source"));
        assert_eq!(list[1].formula, "1 /* ; */ + 2");
        assert_eq!(list[2].formula, "\"a\"\"b;\"");
        let (list, more) = members(text, 2);
        assert!(more);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn broken_text_does_not_panic() {
        for text in [
            "",
            ";;;",
            "shared",
            "shared #\"",
            "[[[",
            "section",
            "shared x",
            "/*",
            "\"",
            "shared #\"a",
        ] {
            let _ = members(text, 10);
        }
    }
}
