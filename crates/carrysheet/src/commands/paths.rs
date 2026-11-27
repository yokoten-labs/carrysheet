//! Showing paths without the user's home folder. Keeping paths inside a
//! root folder is in [`crate::commands::root`].
//!
//! The rules work on text, so tests can check Windows paths on any system:
//! on Windows, letters are compared without case, `\` and `/` are the same
//! separator, and the `\\?\` prefix of verbatim paths is looked through.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The current user's home folder, if the system says.
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|p| !p.as_os_str().is_empty())
}

/// Shows a path with `~` in place of the home folder, so messages, logs and
/// bug reports never carry the user name. Other paths are shown as they are.
pub fn display_path(path: &Path) -> String {
    match home_dir() {
        Some(home) => display_path_with_home(path, &home),
        None => path.display().to_string(),
    }
}

/// [`display_path`] with a given home folder (for tests).
pub fn display_path_with_home(path: &Path, home: &Path) -> String {
    display_path_text(
        &path.display().to_string(),
        &home.display().to_string(),
        cfg!(windows),
    )
}

/// [`display_path`] on text, for Windows (`windows: true`) or Unix rules.
pub fn display_path_text(path: &str, home: &str, windows: bool) -> String {
    let Some(home) = usable_home(home, windows) else {
        return path.to_owned();
    };
    let plain = if windows {
        strip_verbatim(path)
    } else {
        Cow::Borrowed(path)
    };
    match match_prefix(&plain, &home, windows) {
        Some(end) => {
            let rest = &plain[end..];
            match rest.chars().next() {
                None => "~".to_owned(),
                Some(c) if is_separator(c, windows) => format!("~{rest}"),
                // Only part of a name matched, like /home/alicex for /home/alice.
                Some(_) => path.to_owned(),
            }
        }
        None => path.to_owned(),
    }
}

/// Replaces every mention of the home folder in free text with `~`.
pub fn redact_home(text: &str) -> String {
    match home_dir() {
        Some(home) => redact_home_with(text, &home),
        None => text.to_owned(),
    }
}

/// [`redact_home`] with a given home folder (for tests).
pub fn redact_home_with(text: &str, home: &Path) -> String {
    redact_home_text(text, &home.display().to_string(), cfg!(windows))
}

/// [`redact_home`] on text, for Windows (`windows: true`) or Unix rules.
/// Any mention is replaced, even inside a longer name: hiding too much is
/// better than showing a user name.
pub fn redact_home_text(text: &str, home: &str, windows: bool) -> String {
    let Some(home) = usable_home(home, windows) else {
        return text.to_owned();
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i..];
        // A verbatim prefix before the home folder goes too.
        let skip = if windows {
            VERBATIM
                .iter()
                .find(|p| starts_with_ignore_case(rest, p))
                .map_or(0, |p| p.len())
        } else {
            0
        };
        if let Some(len) = match_prefix(&rest[skip..], &home, windows) {
            out.push('~');
            i += skip + len;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// Prefixes of Windows verbatim paths, such as `\\?\C:\Users`.
const VERBATIM: &[&str] = &[r"\\?\UNC\", r"\\?\", "//?/UNC/", "//?/"];

fn is_separator(c: char, windows: bool) -> bool {
    c == '/' || (windows && c == '\\')
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// A path without its verbatim prefix: `\\?\C:\x` is `C:\x` and
/// `\\?\UNC\server\share` is `\\server\share`.
fn strip_verbatim(path: &str) -> Cow<'_, str> {
    for prefix in VERBATIM {
        if starts_with_ignore_case(path, prefix) {
            let rest = &path[prefix.len()..];
            return if prefix.contains("UNC") {
                Cow::Owned(format!(r"\\{rest}"))
            } else {
                Cow::Borrowed(rest)
            };
        }
    }
    Cow::Borrowed(path)
}

/// The home folder as compared: no verbatim prefix, no trailing separator.
/// `None` for a home that would match too much, such as `/` or `C:\`.
fn usable_home(home: &str, windows: bool) -> Option<String> {
    let home = if windows {
        strip_verbatim(home)
    } else {
        Cow::Borrowed(home)
    };
    let trimmed = home.trim_end_matches(|c| is_separator(c, windows));
    let has_separator = trimmed.chars().any(|c| is_separator(c, windows));
    (has_separator && trimmed.chars().count() >= 2).then(|| trimmed.to_owned())
}

/// The length in bytes of `text`'s start that matches `home`, if it does.
fn match_prefix(text: &str, home: &str, windows: bool) -> Option<usize> {
    let mut chars = text.char_indices();
    for h in home.chars() {
        let (_, t) = chars.next()?;
        let same = t == h
            || (windows
                && ((is_separator(t, true) && is_separator(h, true))
                    || t.to_lowercase().eq(h.to_lowercase())));
        if !same {
            return None;
        }
    }
    Some(chars.next().map_or(text.len(), |(i, _)| i))
}
