//! The safety net under the pattern rules in `text.rs`. Patterns will always
//! miss some shape a secret can take, so two broader rules run as well;
//! both hide more than needed rather than risk a leak.
//!
//! 1. **Literals near a keyword.** A keyword is a word that names a secret
//!    (see `keys.rs`: `password`, `pwd`, `token`, `apikey`, any word ending in
//!    `key`, ...) or one of `Authorization`, `Basic`, `Bearer`, `Credential`,
//!    `Credentials`, `Auth`; a string literal holding such a word counts as
//!    a keyword too. The content of every quoted string literal (`"..."` or
//!    `'...'`) among the next six words and literals after a keyword is
//!    masked, so `"Bearer " & "<token>"`,
//!    `"Basic " & Binary.ToText(Text.ToBinary("user:<password>"))` and
//!    `SET PASSWORD FOR 'u' = PASSWORD('<password>')` are hidden. The search
//!    stops at a `,` or `;` outside brackets, at a bracket that closes one
//!    opened before the keyword, or 200 bytes after the keyword.
//! 2. **Random-looking tokens.** A run of at least 24 characters from
//!    `A-Z a-z 0-9 + / = _ - .` is masked whole when it starts with `eyJ`
//!    (base64 of `{"`, the start of every JWT), when its longest run of
//!    letters and digits is 32 or more hex digits, or when that run is 16 or
//!    more characters holding upper case, lower case and at least two digits.
//!    API keys, storage keys, hashes and JWTs take these shapes; words and
//!    GUIDs (`3F2504E0-4F89-11D3-9A0C-0305E82C3301`) do not.

use super::keys::is_secret_key;
use super::text::{Splicer, literal_end, opens_single_quoted};

/// Words and literals looked at after a keyword.
const WINDOW_TOKENS: usize = 6;
/// Bytes looked at after a keyword.
const WINDOW_BYTES: usize = 200;

#[derive(Clone, Copy)]
enum Token {
    /// A run of ASCII letters, digits and `_`.
    Word(usize, usize),
    /// A quoted string literal: the range of its content.
    Literal(usize, usize),
    Open,
    Close,
    /// `,` or `;`.
    Stop,
}

fn tokens(bytes: &[u8]) -> Vec<(usize, Token)> {
    let mut out = Vec::new();
    let mut k = 0;
    // Once a quote finds no end, no later quote of that kind can open a
    // literal worth reading, and scanning again from each would be quadratic.
    let mut unclosed = [false; 2];
    while k < bytes.len() {
        let b = bytes[k];
        if b.is_ascii_alphanumeric() || b == b'_' {
            let start = k;
            while k < bytes.len() && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'_') {
                k += 1;
            }
            out.push((start, Token::Word(start, k)));
            continue;
        }
        let kind = usize::from(b == b'\'');
        if (b == b'"' || (b == b'\'' && opens_single_quoted(bytes, k))) && !unclosed[kind] {
            let start = k + 1;
            let (end, _) = literal_end(bytes, start, b);
            // An unclosed quote is a stray one, not a literal; the pair rules
            // still read any value after it.
            if end < bytes.len() {
                out.push((k, Token::Literal(start, end)));
                k = end + 1;
                continue;
            }
            unclosed[kind] = true;
        }
        match b {
            b'(' | b'[' | b'{' => out.push((k, Token::Open)),
            b')' | b']' | b'}' => out.push((k, Token::Close)),
            b',' | b';' => out.push((k, Token::Stop)),
            _ => {}
        }
        k += 1;
    }
    out
}

/// True for a word after which a secret often follows.
fn is_keyword(word: &str) -> bool {
    is_secret_key(word)
        || [
            "basic",
            "bearer",
            "authorization",
            "credential",
            "credentials",
            "auth",
        ]
        .iter()
        .any(|k| word.eq_ignore_ascii_case(k))
}

/// True when the text holds a keyword as a word of its own.
fn holds_keyword(text: &[u8]) -> bool {
    text.split(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
        .filter(|w| !w.is_empty() && w.len() <= 64)
        .any(|w| is_keyword(&String::from_utf8_lossy(w)))
}

/// Masks the literals that follow a keyword (rule 1).
pub(crate) fn mask_near_keywords(input: &str) -> String {
    let bytes = input.as_bytes();
    let tokens = tokens(bytes);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (i, &(_, token)) in tokens.iter().enumerate() {
        let keyword_end = match token {
            Token::Word(start, end) if end - start <= 64 && is_keyword(&input[start..end]) => end,
            Token::Literal(start, end) if holds_keyword(&bytes[start..end]) => end,
            _ => continue,
        };
        let mut depth = 0usize;
        let mut seen = 0;
        for &(at, next) in &tokens[i + 1..] {
            if at > keyword_end + WINDOW_BYTES || seen >= WINDOW_TOKENS {
                break;
            }
            match next {
                Token::Open => depth += 1,
                Token::Close if depth == 0 => break,
                Token::Close => depth -= 1,
                Token::Stop if depth == 0 => break,
                Token::Stop => {}
                Token::Word(..) => seen += 1,
                Token::Literal(start, end) => {
                    ranges.push((start, end));
                    seen += 1;
                }
            }
        }
    }
    if ranges.is_empty() {
        return input.to_string();
    }
    ranges.sort_unstable();
    ranges.dedup();
    let mut splice = Splicer::new(input);
    for (start, end) in ranges {
        if start >= splice.copied() {
            splice.mask(start, end);
        }
    }
    splice.finish()
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-' | b'.')
}

/// Shortest run that rule 2 looks at.
const MIN_TOKEN: usize = 24;

/// True when `token` looks like a key, hash or JWT (rule 2).
fn looks_random(token: &[u8]) -> bool {
    if token.starts_with(b"eyJ") {
        return true;
    }
    let Some(longest) = token
        .split(|b| !b.is_ascii_alphanumeric())
        .max_by_key(|run| run.len())
    else {
        return false;
    };
    if longest.len() >= 32 && longest.iter().all(u8::is_ascii_hexdigit) {
        return true;
    }
    longest.len() >= 16
        && longest.iter().any(u8::is_ascii_uppercase)
        && longest.iter().any(u8::is_ascii_lowercase)
        && longest.iter().filter(|b| b.is_ascii_digit()).count() >= 2
}

/// Masks long random-looking tokens anywhere (rule 2).
pub(crate) fn mask_random_tokens(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut splice = Splicer::new(input);
    let mut k = 0;
    while k < bytes.len() {
        if !is_token_byte(bytes[k]) {
            k += 1;
            continue;
        }
        let start = k;
        while k < bytes.len() && is_token_byte(bytes[k]) {
            k += 1;
        }
        if k - start >= MIN_TOKEN && looks_random(&bytes[start..k]) {
            splice.mask(start, k);
        }
    }
    splice.finish()
}

#[cfg(test)]
mod tests {
    use super::{looks_random, mask_near_keywords, mask_random_tokens};

    #[test]
    fn random_tokens() {
        for token in [
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc",
            "xK9fP2mQ7vL4sN8wR3tY6uB1/aa+b==",
            "0123456789abcdef0123456789abcdef",
            "ghp_aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5",
        ] {
            assert!(looks_random(token.as_bytes()), "{token}");
        }
        for token in [
            "3F2504E0-4F89-11D3-9A0C-0305E82C3301",
            "ThisWorkbookDataModel_With_Long_Name",
            "Microsoft.Mashup.OleDb.1.Provider.Name",
            "C:/Users/someone/Documents/Reports/2024",
        ] {
            assert!(!looks_random(token.as_bytes()), "{token}");
        }
        assert_eq!(
            mask_random_tokens("key aB3dE5gH7jK9mN1pQ3sT5vW7yZ9 here"),
            "key *** here"
        );
    }

    #[test]
    fn literals_near_keywords() {
        assert_eq!(
            mask_near_keywords(r#"[Authorization="Bearer " & "tok"], [Accept="json"]"#),
            r#"[Authorization="***" & "***"], [Accept="json"]"#
        );
        assert_eq!(
            mask_near_keywords(r#"Text.ToBinary("Basic " & Binary.ToText(Text.ToBinary("u:p")))"#),
            r#"Text.ToBinary("Basic " & Binary.ToText(Text.ToBinary("***")))"#
        );
        assert_eq!(
            mask_near_keywords("SELECT name, city FROM t WHERE city = 'Oslo'"),
            "SELECT name, city FROM t WHERE city = 'Oslo'"
        );
    }
}
