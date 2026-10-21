//! URLs: credentials in the authority (`user:pass@host`), secret query or
//! fragment parameters (`sig=`, `access_token=`, `code=`, `key=`), and URLs
//! nested in a parameter (`?target=ftp://user:pass@host/`).

use super::MASK;
use super::keys::is_secret_key;
use super::text::{MAX_DEPTH, mask_text_at};

/// Masks the secrets in one URL found `depth` levels down. `url` must start
/// with a scheme and `://`; anything else is returned unchanged.
pub(crate) fn mask_url(url: &str, depth: u32) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let authority_start = scheme_end + 3;
    let rest = &url[authority_start..];

    let mut out = String::with_capacity(url.len());
    out.push_str(&url[..authority_start]);
    let after_userinfo = match userinfo_end(rest) {
        Some(at) => {
            let userinfo = &rest[..at];
            match userinfo.find(':') {
                // user:password@ keeps the user name.
                Some(colon) => {
                    out.push_str(&userinfo[..=colon]);
                    out.push_str(MASK);
                }
                // A lone name before `@` is often a token (`https://TOKEN@host`),
                // so it is masked too.
                None => out.push_str(MASK),
            }
            at
        }
        None => 0,
    };
    let rest = &rest[after_userinfo..];
    let host_len = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    out.push_str(&rest[..host_len]);

    let after = &rest[host_len..];
    let (before_fragment, fragment) = match after.find('#') {
        Some(hash) => (&after[..hash], Some(&after[hash + 1..])),
        None => (after, None),
    };
    match before_fragment.find('?') {
        Some(q) => {
            out.push_str(&before_fragment[..=q]);
            out.push_str(&mask_parameters(&before_fragment[q + 1..], depth));
        }
        None => out.push_str(before_fragment),
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(&mask_parameters(fragment, depth));
    }
    out
}

/// Where the user information ends (the `@`), in the text after `://`.
///
/// It is the last `@` before the query or fragment, so a password holding
/// `/` (`user:pa/ss@host`) is still found. When there is none, the first
/// `@` counts if no `=` comes before it (`user:pa?ss@host`). Hiding a path
/// that holds an `@` is the cost of never showing a password.
fn userinfo_end(rest: &str) -> Option<usize> {
    let query = rest.find(['?', '#']).unwrap_or(rest.len());
    rest[..query].rfind('@').or_else(|| {
        let at = rest.find('@')?;
        (!rest[..at].contains('=')).then_some(at)
    })
}

/// Masks secret values in `a=1&b=2` (also `;` separated) parameter lists,
/// keeping the separators as they were. A value that holds a URL or an `@`
/// is decoded and searched again as text.
fn mask_parameters(query: &str, depth: u32) -> String {
    let mut out = String::with_capacity(query.len());
    let mut rest = query;
    loop {
        let (part, separator) = match rest.find(['&', ';']) {
            Some(at) => (&rest[..at], Some(&rest[at..=at])),
            None => (rest, None),
        };
        match part.split_once('=') {
            Some((key, value)) if !value.is_empty() && is_secret_key(&percent_decode(key)) => {
                out.push_str(key);
                out.push('=');
                out.push_str(MASK);
            }
            Some((key, value)) => {
                out.push_str(key);
                out.push('=');
                out.push_str(&mask_nested_value(value, depth));
            }
            None => out.push_str(part),
        }
        match separator {
            Some(separator) => {
                out.push_str(separator);
                rest = &rest[part.len() + 1..];
            }
            None => break,
        }
    }
    out
}

/// A parameter value that is not secret by its key: if it holds a URL or
/// user information (maybe percent-encoded), it is searched again. A value
/// that was encoded is hidden whole when anything in it is secret, since the
/// masked text cannot be encoded back the way it was written.
fn mask_nested_value(value: &str, depth: u32) -> String {
    let decoded = percent_decode(value);
    if !(decoded.contains("://") || decoded.contains('@')) {
        return value.to_string();
    }
    if depth + 1 >= MAX_DEPTH {
        return MASK.to_string();
    }
    let masked = mask_text_at(&decoded, depth + 1);
    if masked == decoded {
        value.to_string()
    } else if decoded == value {
        masked
    } else {
        MASK.to_string()
    }
}

/// Decodes `%XX` escapes, so `access%5Ftoken` is recognized. Invalid
/// escapes are kept as they are.
fn percent_decode(key: &str) -> String {
    let bytes = key.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| char::from(*b).to_digit(16)),
                bytes.get(i + 2).and_then(|b| char::from(*b).to_digit(16)),
            )
        {
            // Two hex digits make at most 255.
            out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
