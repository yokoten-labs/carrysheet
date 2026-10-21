//! Showing paths without the user's home folder. Keeping paths inside a
//! root folder is in [`crate::root`].
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_becomes_tilde() {
        let home = "/home/alice";
        let shown = |p: &str| display_path_text(p, home, false);
        assert_eq!(shown("/home/alice/books/a.xlsx"), "~/books/a.xlsx");
        assert_eq!(shown(home), "~");
        assert_eq!(shown("/home/alice/"), "~/");
        assert_eq!(shown("/home/alicex/a"), "/home/alicex/a");
        assert_eq!(shown("rel/a"), "rel/a");
        // Unix names are case-sensitive.
        assert_eq!(shown("/home/Alice/a"), "/home/Alice/a");
        assert_eq!(display_path_text("/x/a", "/", false), "/x/a");
    }

    #[test]
    fn windows_paths_ignore_case_separators_and_verbatim_prefixes() {
        let home = r"C:\Users\Alice";
        let shown = |p: &str| display_path_text(p, home, true);
        assert_eq!(shown(r"C:\Users\Alice\books\a.xlsx"), r"~\books\a.xlsx");
        assert_eq!(shown(r"c:\users\ALICE\a.xlsx"), r"~\a.xlsx");
        assert_eq!(shown("C:/Users/Alice/a.xlsx"), "~/a.xlsx");
        assert_eq!(shown(r"\\?\C:\Users\Alice\a.xlsx"), r"~\a.xlsx");
        assert_eq!(shown(r"\\?\c:\users\alice"), "~");
        assert_eq!(shown(r"C:\Users\Alicex\a"), r"C:\Users\Alicex\a");
        assert_eq!(shown(r"D:\data\a.xlsx"), r"D:\data\a.xlsx");
        // A verbatim or slash-ended home folder.
        assert_eq!(
            display_path_text(r"C:\Users\Alice\a", r"\\?\C:\Users\Alice\", true),
            r"~\a"
        );
        // A home folder on a network share.
        assert_eq!(
            display_path_text(
                r"\\?\UNC\server\home\alice\a.xlsx",
                r"\\server\home\alice",
                true
            ),
            r"~\a.xlsx"
        );
        // A drive root is not a home folder worth hiding.
        assert_eq!(display_path_text(r"C:\a", r"C:\", true), r"C:\a");
    }

    #[test]
    fn redacts_home_in_text() {
        let home = "/home/alice";
        assert_eq!(
            redact_home_text("failed at /home/alice/a.xlsx and /home/alice", home, false),
            "failed at ~/a.xlsx and ~"
        );
        assert_eq!(
            redact_home_text("/HOME/ALICE/a", home, false),
            "/HOME/ALICE/a"
        );
        assert_eq!(redact_home_text("/x", "/", false), "/x");
        assert_eq!(
            redact_home_text("日本 /home/alice/表", home, false),
            "日本 ~/表"
        );
    }

    #[test]
    fn redacts_windows_style_home() {
        let home = r"C:\Users\alice";
        assert_eq!(
            redact_home_text(
                r"C:\Users\alice\a.xlsx or C:/Users/alice/b.xlsx or c:\USERS\Alice\c",
                home,
                true
            ),
            r"~\a.xlsx or ~/b.xlsx or ~\c"
        );
        assert_eq!(
            redact_home_text(r"open \\?\C:\Users\alice\d.xlsx failed", home, true),
            r"open ~\d.xlsx failed"
        );
        assert_eq!(
            redact_home_text(r"x C:\Users\alice\e", r"\\?\C:\Users\alice", true),
            r"x ~\e"
        );
        assert_eq!(redact_home_text(r"C:\a C:\b", r"C:\", true), r"C:\a C:\b");
    }

    #[test]
    fn real_home_is_hidden() {
        if let Some(home) = home_dir() {
            let p = home.join("x.xlsx");
            let shown = display_path(&p);
            assert!(shown.starts_with('~'), "{shown}");
            assert!(!shown.contains(&home.display().to_string()));
            let text = format!("see {}", p.display());
            assert!(!redact_home(&text).contains(&home.display().to_string()));
        }
    }
}
