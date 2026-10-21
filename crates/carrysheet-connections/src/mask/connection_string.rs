//! Connection strings: `key=value;` pairs as ODBC and OLE DB write them.
//!
//! The parser follows the published rules for both families:
//! - pairs are separated by `;`, and spaces (line breaks too) around keys
//!   and values are ignored;
//! - an OLE DB value may be quoted with `"` or `'`, and the quote character is
//!   doubled to appear inside (`"a""b"`); a `;` inside quotes is part of the
//!   value;
//! - an ODBC value may be wrapped in `{}`, and `}}` stands for one `}` inside;
//! - an OLE DB key writes `=` as `==`.
//!
//! The output keeps the original text and replaces only secret values with
//! `***`. Values that are not secret are still searched for secrets, because
//! they may hold a nested connection string (`Extended Properties="...;PWD=x"`)
//! or a URL with credentials.

use super::MASK;
use super::keys::is_secret_key;
use super::text::mask_text;

/// How deep nested connection strings are followed before the value is
/// searched as plain text instead.
const MAX_NESTING: u32 = 4;

/// Longest key checked for the ODBC reading of `==`.
const MAX_KEY_BYTES: usize = 64;

/// Masks every secret in a connection string.
pub fn mask_connection_string(input: &str) -> String {
    mask_nested(input, 0)
}

fn mask_nested(input: &str, depth: u32) -> String {
    if depth > MAX_NESTING {
        return mask_text(input);
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        // Separators and spaces between pairs are copied as they are.
        if bytes[i] == b';' || bytes[i].is_ascii_whitespace() {
            out.push(char::from(bytes[i]));
            i += 1;
            continue;
        }
        let Some(eq) = find_key_end(bytes, i) else {
            // A segment without `=`: not a pair. Search it as text up to the
            // next `;`.
            let end = find_byte(bytes, i, b';');
            out.push_str(&mask_text(&input[i..end]));
            i = end;
            continue;
        };
        let key = &input[i..eq];
        out.push_str(key);
        out.push('=');
        let mut j = eq + 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            out.push(char::from(bytes[j]));
            j += 1;
        }
        let value_end = value_end(bytes, j);
        let value = &input[j..value_end];
        if is_secret_key(key) {
            if !value.trim().is_empty() {
                out.push_str(MASK);
            }
        } else {
            out.push_str(&mask_value(value, depth));
        }
        i = value_end;
    }
    out
}

/// Finds the `=` that ends the key starting at `start`, skipping `==` (an
/// escaped `=` inside an OLE DB key). Returns `None` when the segment has no
/// `=` before the next `;`.
///
/// ODBC has no such escape: `PWD==x` is the key `PWD` with the value `=x`.
/// So when the key before a `==` is already secret, the first `=` ends it,
/// and the value is masked either way.
fn find_key_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut k = start;
    while k < bytes.len() {
        match bytes[k] {
            b';' => return None,
            b'=' if bytes.get(k + 1) == Some(&b'=')
                && k - start <= MAX_KEY_BYTES
                && is_secret_key(&String::from_utf8_lossy(&bytes[start..k])) =>
            {
                return Some(k);
            }
            b'=' if bytes.get(k + 1) == Some(&b'=') => k += 2,
            b'=' => return Some(k),
            _ => k += 1,
        }
    }
    None
}

/// Where the value that starts at `start` ends: after its closing quote or
/// brace and anything up to the next `;`, or at the next `;`. An unclosed
/// quote or brace runs to the end of the string, so a broken string can only
/// hide more, never less.
fn value_end(bytes: &[u8], start: usize) -> usize {
    let closed = match bytes.get(start) {
        Some(&quote @ (b'"' | b'\'')) => closing(bytes, start + 1, quote),
        Some(b'{') => closing(bytes, start + 1, b'}'),
        _ => Some(start),
    };
    match closed {
        Some(after) => find_byte(bytes, after, b';'),
        None => bytes.len(),
    }
}

/// Position just after the `close` byte that ends a quoted or braced value
/// starting at `from`; a doubled `close` is an escaped one.
fn closing(bytes: &[u8], from: usize, close: u8) -> Option<usize> {
    let mut k = from;
    while k < bytes.len() {
        if bytes[k] == close {
            if bytes.get(k + 1) == Some(&close) {
                k += 2;
                continue;
            }
            return Some(k + 1);
        }
        k += 1;
    }
    None
}

fn find_byte(bytes: &[u8], from: usize, needle: u8) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| b == needle)
        .map_or(bytes.len(), |p| from + p)
}

/// Masks secrets inside a value that is not itself secret: a quoted or braced
/// value is unwrapped, searched as a nested connection string, and wrapped
/// again; anything else is searched as text (URLs, `key=value`).
fn mask_value(value: &str, depth: u32) -> String {
    let trimmed_end = value.trim_end();
    let tail = &value[trimmed_end.len()..];
    let (open, close) = match trimmed_end.as_bytes().first() {
        Some(b'"') => ('"', '"'),
        Some(b'\'') => ('\'', '\''),
        Some(b'{') => ('{', '}'),
        _ => return mask_text(value),
    };
    let Some(inner) = trimmed_end
        .strip_prefix(open)
        .and_then(|v| v.strip_suffix(close))
    else {
        // Unclosed or followed by junk before `;`: search it all as text.
        return mask_text(value);
    };
    let doubled: String = [close, close].iter().collect();
    let unescaped = inner.replace(&doubled, &close.to_string());
    let masked = mask_nested(&unescaped, depth + 1);
    if masked == unescaped {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    out.push(open);
    out.push_str(&masked.replace(close, &doubled));
    out.push(close);
    out.push_str(tail);
    out
}

/// Splits a connection string into (key, value) pairs with quotes and braces
/// removed. For reading targets inside this crate only: the values are raw
/// and must be cleaned before they leave it.
pub(crate) fn pairs(input: &str) -> Vec<(String, String)> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b';' || bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let Some(eq) = find_key_end(bytes, i) else {
            i = find_byte(bytes, i, b';');
            continue;
        };
        let key = input[i..eq].trim().replace("==", "=");
        let mut j = eq + 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let end = value_end(bytes, j);
        out.push((key, unwrap_value(input[j..end].trim_end())));
        i = end;
    }
    out
}

/// Removes the quotes or braces around a value and undoes doubling.
fn unwrap_value(value: &str) -> String {
    for (open, close) in [('"', '"'), ('\'', '\''), ('{', '}')] {
        if let Some(inner) = value.strip_prefix(open).and_then(|v| v.strip_suffix(close)) {
            let doubled: String = [close, close].iter().collect();
            return inner.replace(&doubled, &close.to_string());
        }
    }
    value.to_string()
}
