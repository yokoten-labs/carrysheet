//! Free text (SQL command text, Power Query M formulas, names, paths):
//! finds the shapes secrets take and masks them.
//!
//! Passes, in order (each one only ever hides more):
//! 1. string literals whose content holds an escaped quote (`""` in M, `''`
//!    in SQL) are unescaped, masked on their own with all of these passes,
//!    and escaped again, so an M-escaped JSON body or connection string is
//!    read the way the program will read it;
//! 2. the safety net in `nearby.rs`: quoted literals that follow a secret key
//!    or `Authorization`, `Basic`, `Bearer`, `Credential`;
//! 3. URLs (`scheme://...`): credentials and secret parameters, see `url.rs`;
//! 4. `key = value` pairs where the key is secret. The separator is `=`, `:`,
//!    `:=`, `=>` or `==`, and the value starts after any spaces or line
//!    breaks. It is `""..""` (an escaped string inside an M string),
//!    `"..."` or `'...'` (a doubled quote is an escaped one), `{...}` (`}}`
//!    is an escaped brace), or bare: up to the next `;`, line break, or a
//!    lone `"` (the end of the string literal it sits in; a doubled `""` is
//!    an escaped quote and does not end it). Bare values do not end at `&`,
//!    `'`, `<` or `>`, so a password holding them is hidden in full;
//! 5. XML elements named like a secret key: everything up to the closing
//!    tag, CDATA included;
//! 6. `Bearer <token>`, `Basic <token>`, `IDENTIFIED BY <secret>`,
//!    `PASSWORD '<secret>'` and `PASSWORD('<secret>')` anywhere;
//! 7. the second half of the safety net in `nearby.rs`: long tokens that look
//!    random (keys, hashes, JWTs) anywhere.

use super::MASK;
use super::keys::is_secret_key;
use super::nearby::{mask_near_keywords, mask_random_tokens};
use super::url::mask_url;

/// How deep string literals and URLs inside URLs are unescaped and searched
/// again. Past it, a nested URL parameter that holds `://` or `@` is hidden
/// whole.
pub(crate) const MAX_DEPTH: u32 = 3;

/// Masks the secrets in free text: SQL, Power Query M, names, paths.
///
/// Pattern rules hide the value after a secret key (`password`, `pwd`,
/// `token`, `secret`, any key ending in `key` or `pass`, ...) written as
/// `key=value`, `key: value`, `key := value`, `key => value` or
/// `<key>value</key>`; credentials and secret parameters in URLs, also in
/// URLs nested in a parameter; and the token after `Bearer`, `Basic`,
/// `IDENTIFIED BY` or `PASSWORD`. String literals holding escaped quotes
/// (`""` in M, `''` in SQL) are unescaped and searched again, so a
/// connection string or JSON body inside an M string is read the way the
/// program reads it.
///
/// Under the patterns is a safety net that hides more than needed rather
/// than risk a leak:
/// - every quoted string literal within six words or literals after a
///   secret key or `Authorization`, `Basic`, `Bearer`, `Credential`
///   (stopping at a `,` or `;` outside brackets);
/// - every run of 24 or more `A-Z a-z 0-9 + / = _ - .` that looks random: it
///   starts with `eyJ` (a JWT), holds 32 or more hex digits in a row, or
///   holds 16 or more letters and digits in a row with upper case, lower
///   case and at least two digits.
///
/// Masked values become [`crate::MASK`]; the keys and the text around them
/// stay.
pub fn mask_text(input: &str) -> String {
    mask_text_at(input, 0)
}

/// [`mask_text`] for text found inside other text, `depth` levels down.
pub(crate) fn mask_text_at(input: &str, depth: u32) -> String {
    let first = if depth < MAX_DEPTH {
        mask_literals(input, depth)
    } else {
        input.to_string()
    };
    other_passes(first, depth)
}

/// [`mask_text`] that drops `input` once the first pass has copied it.
pub(crate) fn mask_text_owned(input: String) -> String {
    let first = mask_literals(&input, 0);
    drop(input);
    other_passes(first, 0)
}

/// The passes after the first. Each one replaces `text`, so only two copies
/// are alive at a time.
fn other_passes(mut text: String, depth: u32) -> String {
    text = mask_near_keywords(&text);
    text = mask_urls(&text, depth);
    text = mask_pairs(&text);
    text = mask_tags(&text);
    text = mask_phrases(&text);
    mask_random_tokens(&text)
}

/// Builds the output of one pass: copies the input and swaps ranges for
/// `***`. Ranges must come in order and must not overlap.
pub(crate) struct Splicer<'a> {
    input: &'a str,
    out: String,
    copied: usize,
}

impl<'a> Splicer<'a> {
    pub(crate) fn new(input: &'a str) -> Self {
        Self {
            input,
            out: String::with_capacity(input.len()),
            copied: 0,
        }
    }

    /// End of the last replaced range.
    pub(crate) fn copied(&self) -> usize {
        self.copied
    }

    /// Replaces `input[start..end]` with `replacement`.
    pub(crate) fn replace(&mut self, start: usize, end: usize, replacement: &str) {
        self.out.push_str(&self.input[self.copied..start]);
        self.out.push_str(replacement);
        self.copied = end;
    }

    /// Masks `input[start..end]` unless it is empty or already masked.
    pub(crate) fn mask(&mut self, start: usize, end: usize) {
        let value = &self.input[start..end];
        if value.trim().is_empty() || value == MASK {
            return;
        }
        self.replace(start, end, MASK);
    }

    pub(crate) fn finish(mut self) -> String {
        self.out.push_str(&self.input[self.copied..]);
        self.out
    }
}

/// End of a string literal whose content starts at `from`: the position of
/// the closing `quote` (or the end of the text when it is not closed), and
/// whether a doubled, escaped quote was seen inside.
///
/// A backslash before the quote or before another backslash escapes it too
/// (JSON, C-like languages), except before a doubled quote: `\""` is an
/// escaped JSON quote inside an M string, so the doubling wins. Where a backslash is just a backslash
/// (`"C:\dir\"` in M), the literal is read as longer than it is, which can
/// only hide more. Every pass uses this one rule, so a range one pass masks
/// never splits a value the next pass reads.
pub(crate) fn literal_end(bytes: &[u8], from: usize, quote: u8) -> (usize, bool) {
    let mut k = from;
    let mut escaped = false;
    while k < bytes.len() {
        if bytes[k] == b'\\' {
            let next = bytes.get(k + 1).copied();
            if next == Some(b'\\')
                || (next == Some(quote) && bytes.get(k + 2).copied() != Some(quote))
            {
                k += 2;
                continue;
            }
        }
        if bytes[k] == quote {
            if bytes.get(k + 1) == Some(&quote) {
                escaped = true;
                k += 2;
                continue;
            }
            return (k, escaped);
        }
        k += 1;
    }
    (bytes.len(), escaped)
}

/// True when a `'` at `at` can open a string literal: not an apostrophe
/// inside a word (`it's`).
pub(crate) fn opens_single_quoted(bytes: &[u8], at: usize) -> bool {
    at == 0 || !bytes[at - 1].is_ascii_alphanumeric()
}

fn mask_literals(input: &str, depth: u32) -> String {
    let text = mask_literals_of(input, b'"', depth);
    mask_literals_of(&text, b'\'', depth)
}

fn mask_literals_of(input: &str, quote: u8, depth: u32) -> String {
    let bytes = input.as_bytes();
    let single = char::from(quote).to_string();
    let doubled = single.repeat(2);
    let mut splice = Splicer::new(input);
    let mut k = 0;
    while k < bytes.len() {
        if bytes[k] != quote || (quote == b'\'' && !opens_single_quoted(bytes, k)) {
            k += 1;
            continue;
        }
        let start = k + 1;
        let (end, escaped) = literal_end(bytes, start, quote);
        if escaped {
            let unescaped = input[start..end].replace(&doubled, &single);
            let masked = mask_text_at(&unescaped, depth + 1);
            if masked != unescaped {
                splice.replace(start, end, &masked.replace(&single, &doubled));
            }
        }
        k = end + 1;
    }
    splice.finish()
}

/// Bytes that end a URL found in running text.
fn ends_url(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'<' | b'>' | b'`' | b'{' | b'}' | b'|')
}

/// The URLs (`scheme://...`) in `bytes`, in order, as byte ranges.
fn find_urls(input: &str) -> Vec<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut urls = Vec::new();
    let mut from = 0;
    let mut last_end = 0;
    while let Some(found) = input[from..].find("://") {
        let colon = from + found;
        let mut start = colon;
        while start > last_end
            && (bytes[start - 1].is_ascii_alphanumeric()
                || matches!(bytes[start - 1], b'+' | b'.' | b'-'))
        {
            start -= 1;
        }
        let mut end = colon + 3;
        while end < bytes.len() && !ends_url(bytes[end]) {
            end += 1;
        }
        if start < colon {
            urls.push((start, end));
            last_end = end;
        }
        from = end.max(colon + 3);
    }
    urls
}

fn mask_urls(input: &str, depth: u32) -> String {
    let mut splice = Splicer::new(input);
    for (start, end) in find_urls(input) {
        let masked = mask_url(&input[start..end], depth);
        splice.replace(start, end, &masked);
    }
    splice.finish()
}

/// Bytes allowed in an unquoted key read backwards from `=` or `:`. Spaces
/// are allowed so `User Password` and `Jet OLEDB:Database Password` work.
fn is_key_byte(b: u8, colon_allowed: bool) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(b, b'_' | b'.' | b'-' | b' ' | b'\t')
        || (b == b':' && colon_allowed)
}

/// Longest key read backwards, in bytes.
const MAX_KEY_BYTES: usize = 64;

/// True when the key that ends at `at` (exclusive) is secret. A `:` may be
/// part of the key only when the separator is not `:` itself.
fn secret_key_before(bytes: &[u8], at: usize, colon_separator: bool) -> bool {
    let mut end = at;
    while end > 0 && matches!(bytes[end - 1], b' ' | b'\t') {
        end -= 1;
    }
    if end == 0 {
        return false;
    }
    let last = bytes[end - 1];
    if matches!(last, b'"' | b'\'' | b']') {
        // A quoted key: "password", 'pwd', #"api-key", [Password], and
        // ""password"" inside an M string.
        let open = if last == b']' { b'[' } else { last };
        let mut inner_end = end - 1;
        if last != b']' && inner_end > 0 && bytes[inner_end - 1] == last {
            inner_end -= 1;
        }
        let lower = inner_end.saturating_sub(MAX_KEY_BYTES);
        let Some(open_at) = bytes[lower..inner_end].iter().rposition(|&b| b == open) else {
            return false;
        };
        let key = String::from_utf8_lossy(&bytes[lower + open_at + 1..inner_end]);
        return is_secret_key(&key);
    }
    if colon_separator && !last.is_ascii_alphanumeric() {
        return false;
    }
    let mut start = end;
    while start > 0
        && end - start < MAX_KEY_BYTES
        && is_key_byte(bytes[start - 1], !colon_separator)
    {
        start -= 1;
    }
    let run = String::from_utf8_lossy(&bytes[start..end]);
    let words: Vec<&str> = run.split_whitespace().collect();
    match words.as_slice() {
        [] => false,
        [one] => is_secret_key(one),
        [.., two, one] => is_secret_key(one) || is_secret_key(&format!("{two}{one}")),
    }
}

/// A separator at `p`: where the key before it ends, where the value after
/// it starts, and whether it is a plain `:`.
fn separator_at(bytes: &[u8], p: usize) -> Option<(usize, usize, bool)> {
    let next = bytes.get(p + 1).copied();
    let prev = if p > 0 { bytes[p - 1] } else { 0 };
    match bytes[p] {
        b'=' if matches!(prev, b'=' | b'!' | b'<' | b'>') => None,
        b'=' if matches!(next, Some(b'=' | b'>')) => Some((p, p + 2, false)),
        b'=' if prev == b':' => Some((p - 1, p + 1, false)),
        b'=' => Some((p, p + 1, false)),
        b':' if matches!(next, Some(b'/' | b'\\' | b'=')) => None,
        b':' => Some((p, p + 1, true)),
        _ => None,
    }
}

/// Finds the value that starts at or after `from` (after spaces and line
/// breaks): returns the range to mask. Inside a URL (`in_url` is where it
/// ends), a bare value also ends at `&` or `#`, which separate parameters
/// there.
fn value_range(bytes: &[u8], from: usize, in_url: Option<usize>) -> Option<(usize, usize)> {
    let mut v = from;
    while v < bytes.len() && bytes[v].is_ascii_whitespace() {
        v += 1;
    }
    let first = *bytes.get(v)?;
    if matches!(first, b'"' | b'\'')
        && bytes.get(v + 1) == Some(&first)
        && bytes.get(v + 2) != Some(&first)
    {
        // ""value"" or ''value'' inside an M or SQL string literal.
        let start = v + 2;
        return Some((start, find_pair(bytes, start, first).unwrap_or(bytes.len())));
    }
    if matches!(first, b'"' | b'\'' | b'{') {
        let close = if first == b'{' { b'}' } else { first };
        let start = v + 1;
        return Some((start, literal_end(bytes, start, close).0));
    }
    let limit = in_url.unwrap_or(bytes.len());
    let mut end = v;
    while end < limit {
        match bytes[end] {
            b';' | b'\n' | b'\r' => break,
            b'&' | b'#' if in_url.is_some() => break,
            b'"' if bytes.get(end + 1) == Some(&b'"') => end += 2,
            b'"' => break,
            _ => end += 1,
        }
    }
    let end = end.min(bytes.len());
    // Trailing spaces before a separator are not part of the value.
    let mut trimmed = end;
    while trimmed > v && matches!(bytes[trimmed - 1], b' ' | b'\t') {
        trimmed -= 1;
    }
    Some((v, trimmed))
}

/// Position of the doubled `quote` that ends a `""value""` starting at
/// `from`. Four quotes in a row, or a doubled quote after a backslash
/// (`\""`, a JSON escape inside an M string), are escaped quotes inside it.
fn find_pair(bytes: &[u8], from: usize, quote: u8) -> Option<usize> {
    let mut k = from;
    while k + 1 < bytes.len() {
        if bytes[k] == quote && bytes[k + 1] == quote && backslashes_before(bytes, k) % 2 == 1 {
            k += 2;
            continue;
        }
        if bytes[k] == quote && bytes[k + 1] == quote {
            if bytes.get(k + 2) == Some(&quote) && bytes.get(k + 3) == Some(&quote) {
                k += 4;
                continue;
            }
            return Some(k);
        }
        k += 1;
    }
    None
}

/// How many backslashes come right before `at`.
fn backslashes_before(bytes: &[u8], at: usize) -> usize {
    bytes[..at]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count()
}

fn mask_pairs(input: &str) -> String {
    let bytes = input.as_bytes();
    let urls = find_urls(input);
    let mut next_url = 0;
    let mut splice = Splicer::new(input);
    let mut p = 0;
    while p < bytes.len() {
        while next_url < urls.len() && urls[next_url].1 <= p {
            next_url += 1;
        }
        let in_url = urls
            .get(next_url)
            .filter(|(start, _)| *start <= p)
            .map(|(_, end)| *end);
        if let Some((key_end, value_from, colon)) = separator_at(bytes, p)
            && key_end >= splice.copied()
            && secret_key_before(bytes, key_end, colon)
            && let Some((start, end)) = value_range(bytes, value_from, in_url)
        {
            splice.mask(start, end);
            p = end.max(p + 1);
            continue;
        }
        p += 1;
    }
    splice.finish()
}

/// Bytes that may appear in an XML element name.
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':')
}

/// Position of `</name` (ASCII case ignored) from `from`.
fn find_closing_tag(bytes: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut k = from;
    while k + 2 + name.len() <= bytes.len() {
        if bytes[k] == b'<'
            && bytes[k + 1] == b'/'
            && bytes[k + 2..k + 2 + name.len()].eq_ignore_ascii_case(name)
        {
            return Some(k);
        }
        k += 1;
    }
    None
}

fn mask_tags(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut splice = Splicer::new(input);
    let mut from = 0;
    while let Some(found) = input[from..].find('<') {
        let open = from + found;
        let name_start = open + 1;
        let mut name_end = name_start;
        while name_end < bytes.len() && is_name_byte(bytes[name_end]) {
            name_end += 1;
        }
        if name_end == name_start {
            // A stray `<` (a comparison, `</`, `<!`): look again just after it.
            from = open + 1;
            continue;
        }
        let mut close = name_end;
        while close < bytes.len() && !matches!(bytes[close], b'>' | b'<') {
            close += 1;
        }
        if close >= bytes.len() {
            break;
        }
        if bytes[close] == b'<' {
            // Not a tag after all; the next `<` may start a real one.
            from = close;
            continue;
        }
        from = close + 1;
        if bytes[close - 1] == b'/' {
            continue;
        }
        let name = &input[name_start..name_end];
        let local = name.rsplit(':').next().unwrap_or(name);
        if is_secret_key(local) {
            let content_end =
                find_closing_tag(bytes, close + 1, name.as_bytes()).unwrap_or(bytes.len());
            splice.mask(close + 1, content_end);
            from = content_end.max(close + 1);
        }
    }
    splice.finish()
}

/// Finds `needle` (ASCII, lower case) in `haystack` ignoring ASCII case,
/// starting at a word boundary, from `from`.
fn find_word(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&k| {
        haystack[k..k + needle.len()].eq_ignore_ascii_case(needle)
            && (k == 0 || !haystack[k - 1].is_ascii_alphanumeric())
            && haystack
                .get(k + needle.len())
                .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_')
    })
}

/// Bytes that end a token after `Bearer` or `IDENTIFIED BY`.
fn ends_token(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b';' | b'&' | b',' | b'"' | b'\'' | b')' | b']' | b'}' | b'<' | b'>'
        )
}

fn skip_spaces(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
        at += 1;
    }
    at
}

/// What may follow a phrase.
#[derive(Clone, Copy, PartialEq)]
enum After {
    /// A token of at least this many bytes, or a quoted value.
    Token(usize),
    /// Only a quoted value, maybe inside parentheses: `PASSWORD 'x'`,
    /// `PASSWORD('x')`.
    Quoted,
}

fn mask_phrases(input: &str) -> String {
    let bytes = input.as_bytes();
    let phrases: [(&[u8], After); 5] = [
        (b"bearer", After::Token(8)),
        (b"basic", After::Token(4)),
        (b"identified by", After::Token(1)),
        (b"password", After::Quoted),
        (b"pwd", After::Quoted),
    ];
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (phrase, after) in phrases {
        let mut from = 0;
        while let Some(at) = find_word(bytes, phrase, from) {
            let word_end = at + phrase.len();
            from = word_end;
            let mut v = skip_spaces(bytes, word_end);
            if after == After::Quoted && bytes.get(v) == Some(&b'(') {
                v = skip_spaces(bytes, v + 1);
            } else if v == word_end {
                // `"password":` is a quoted key, not the phrase.
                continue;
            }
            let range = match (bytes.get(v), after) {
                // After `Bearer` or `Basic` a quote more often closes the
                // literal the word sits in (`"Basic " & ...`); the safety net
                // covers `Bearer "token"`.
                (Some(b'"' | b'\''), After::Token(_)) => None,
                (Some(&q @ (b'"' | b'\'')), After::Quoted) => {
                    let start = v + 1;
                    Some((start, literal_end(bytes, start, q).0))
                }
                (Some(_), After::Token(min_len)) => {
                    let mut end = v;
                    while end < bytes.len() && !ends_token(bytes[end]) {
                        end += 1;
                    }
                    (end - v >= min_len).then_some((v, end))
                }
                _ => None,
            };
            if let Some(range) = range {
                ranges.push(range);
            }
        }
    }
    ranges.sort_unstable();
    let mut splice = Splicer::new(input);
    for (start, end) in ranges {
        if start >= splice.copied() {
            splice.mask(start, end);
        }
    }
    splice.finish()
}
