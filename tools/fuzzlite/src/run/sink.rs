//! The only way out: every byte of text fuzzlite writes to stdout, stderr
//! or a report file goes through this module, which hides the home folder
//! first. Call sites cannot forget it, because the crate denies
//! `println!`/`eprintln!` and `std::io::stdout` everywhere else.
//!
//! Finding inputs (`.bin`) are fuzz data, written as they are.

use std::io::Write as _;
use std::path::Path;
use std::sync::OnceLock;

/// Writes one line to stdout (results), after [`scrub`].
#[allow(clippy::disallowed_methods, clippy::print_stdout)]
pub fn stdout_line(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", scrub(line));
    let _ = out.flush();
}

/// Writes one line to stderr (messages, progress), after [`scrub`].
#[allow(clippy::print_stderr)]
pub fn stderr_line(line: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{}", scrub(line));
}

/// Writes a text file (a finding's JSON), after [`scrub`].
pub fn write_text(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, scrub(text))
}

/// `say!(...)`: `format!` to stderr through the sink.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::run::sink::stderr_line(&format!($($arg)*))
    };
}

/// Windows' verbatim prefixes, which `canonicalize` adds there.
const VERBATIM: &str = r"\\?\";
const VERBATIM_UNC: &str = r"\\?\UNC\";

/// Hides the home folder in `text`: `/home/<user>/x` becomes `~/x` and
/// `\\?\C:\Users\<user>\x` becomes `~\x`. The home is taken from `HOME`
/// and `USERPROFILE`, each also in its canonical form (a symlinked home).
pub fn scrub(text: &str) -> String {
    static HOMES: OnceLock<Vec<String>> = OnceLock::new();
    let homes = HOMES.get_or_init(|| {
        let mut homes = Vec::new();
        for key in ["HOME", "USERPROFILE"] {
            let Some(h) = std::env::var_os(key).filter(|h| !h.is_empty()) else {
                continue;
            };
            homes.push(h.to_string_lossy().into_owned());
            if let Ok(real) = std::fs::canonicalize(&h) {
                homes.push(real.to_string_lossy().into_owned());
            }
        }
        homes
    });
    scrub_with(text, homes)
}

/// [`scrub`] with the home folders given, for tests. A home that looks
/// like a Windows path (`C:\...`) matches without regard to case, with
/// either slash, and in Rust's `Debug` form with doubled backslashes.
pub fn scrub_with(text: &str, homes: &[String]) -> String {
    let mut out = text
        .replace(VERBATIM_UNC, r"\\")
        .replace(r"\\\\?\\UNC\\", r"\\\\")
        .replace(VERBATIM, "")
        .replace(r"\\\\?\\", "");
    let mut variants: Vec<(String, bool)> = Vec::new();
    for h in homes {
        let h = h.strip_prefix(VERBATIM).unwrap_or(h);
        let h = h.trim_end_matches(['/', '\\']);
        if h.len() < 2 {
            continue;
        }
        let windows = h.as_bytes().get(1) == Some(&b':') || h.contains('\\');
        variants.push((h.to_string(), windows));
        if windows {
            let back = h.replace('/', "\\");
            variants.push((back.replace('\\', "/"), true));
            variants.push((back.replace('\\', r"\\"), true));
            variants.push((back, true));
        }
    }
    // Longest first, so `/home/a` never eats part of `/home/ab`.
    variants.sort_by_key(|(v, _)| std::cmp::Reverse(v.len()));
    variants.dedup();
    for (v, any_case) in &variants {
        out = replace_dir(&out, v, *any_case);
    }
    out
}

/// Replaces `dir` with `~` where it is a whole path prefix: not followed by
/// a letter, a digit, `_` or `-`.
fn replace_dir(text: &str, dir: &str, any_case: bool) -> String {
    let (hay, needle) = (text.as_bytes(), dir.as_bytes());
    let same = |a: &[u8]| {
        if any_case {
            a.eq_ignore_ascii_case(needle)
        } else {
            a == needle
        }
    };
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        let end = i + needle.len();
        if end <= hay.len() && same(&hay[i..end]) {
            let whole = hay
                .get(end)
                .is_none_or(|&c| !(c.is_ascii_alphanumeric() || c == b'_' || c == b'-'));
            if whole {
                out.push(b'~');
                i = end;
                continue;
            }
        }
        out.push(hay[i]);
        i += 1;
    }
    // Only whole ASCII-matching runs were replaced, so this stays UTF-8.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn homes(h: &[&str]) -> Vec<String> {
        h.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hides_a_linux_home() {
        let h = homes(&["/home/ada"]);
        let s = |t: &str| scrub_with(t, &h);
        assert_eq!(s("/home/ada/.cache/x.bin"), "~/.cache/x.bin");
        assert_eq!(
            s("/home/ada/.cargo/registry/src/cfb-0.15.0/src/lib.rs:10:5: oops"),
            "~/.cargo/registry/src/cfb-0.15.0/src/lib.rs:10:5: oops"
        );
        assert_eq!(s("/home/adam/x"), "/home/adam/x");
        assert_eq!(s("/home/ada_2/x"), "/home/ada_2/x");
        assert_eq!(s("/home/ada"), "~");
        assert_eq!(s("x=/home/ada,"), "x=~,");
        assert_eq!(s("\"/home/ada\""), "\"~\"");
        assert_eq!(s("[/home/ada]"), "[~]");
        assert_eq!(s("/home/ada;"), "~;");
        assert_eq!(s("/home/ada."), "~.");
        // Linux paths are case-sensitive: another folder.
        assert_eq!(s("/HOME/ADA/x"), "/HOME/ADA/x");
    }

    #[test]
    fn hides_a_windows_home_in_every_spelling() {
        let h = homes(&[r"\\?\C:\Users\x\"]);
        let s = |t: &str| scrub_with(t, &h);
        assert_eq!(
            s(r"\\?\C:\Users\x\AppData\Local\carrysheet-fuzz\biff\a.bin"),
            r"~\AppData\Local\carrysheet-fuzz\biff\a.bin"
        );
        assert_eq!(
            s(r"C:\Users\x\.cargo\registry\cfb-0.10.0\src\lib.rs:1:1"),
            r"~\.cargo\registry\cfb-0.10.0\src\lib.rs:1:1"
        );
        assert_eq!(
            s(r#"Err(Os { path: "C:\\Users\\x\\seeds\\a.xls" })"#),
            r#"Err(Os { path: "~\\seeds\\a.xls" })"#
        );
        assert_eq!(s(r"c:\users\x\seeds\a.xls"), r"~\seeds\a.xls");
        assert_eq!(s(r"C:\USERS\X\seeds"), r"~\seeds");
        assert_eq!(s("C:/Users/x/seeds"), "~/seeds");
        assert_eq!(s(r"file C:\Users\x."), r"file ~.");
        assert_eq!(s(r"C:\Users\x,C:\Users\x;"), "~,~;");
        assert_eq!(s(r"\\?\C:\Users\x"), "~");
        assert_eq!(s(r"C:\Users\xy\a"), r"C:\Users\xy\a");
        // UNC paths keep their leading backslashes.
        assert_eq!(s(r"\\?\UNC\server\share\x"), r"\\server\share\x");
    }
}
