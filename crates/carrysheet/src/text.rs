//! Text from a file, made safe to show (issue 67).
//!
//! A file can hold characters a person does not see but a program or a
//! language model reads: terminal escapes, direction overrides, zero-width
//! characters, tag characters, private use characters. Such text never
//! reaches a message, a JSON result or a terminal raw.
//!
//! - [`escape`]: hidden characters ([`HIDDEN`]) become `\u{1b}` (lowercase
//!   hex), a backslash becomes `\\`, so text cannot fake an escape.
//! - [`quote`]: the same in double quotes, with `"` written as `\"`.
//!
//! Both take a limit in characters of output. A longer text is cut before
//! the escape that would not fit, never inside one, and ends with [`CUT`]
//! and how many characters of the input were left out.

use std::fmt::Write as _;

/// Starts the mark after a cut text: `…[cut: 12 more characters]`.
pub const CUT: &str = "…[cut: ";

/// Hidden code points (general category Cc, Cf, Co, Zl or Zp, or
/// Default_Ignorable_Code_Point) as inclusive, sorted ranges that do not
/// touch. Made from the Unicode 18.0.0 files DerivedGeneralCategory.txt and
/// DerivedCoreProperties.txt; merge them again at a new release.
#[rustfmt::skip]
pub const HIDDEN: [(u32, u32); 30] = [
    (0x0000, 0x001F), (0x007F, 0x009F), (0x00AD, 0x00AD), (0x034F, 0x034F),
    (0x0600, 0x0605), (0x061C, 0x061C), (0x06DD, 0x06DD), (0x070F, 0x070F),
    (0x0890, 0x0891), (0x08E2, 0x08E2), (0x115F, 0x1160), (0x17B4, 0x17B5),
    (0x180B, 0x180F), (0x200B, 0x200F), (0x2028, 0x202E), (0x2060, 0x206F),
    (0x3164, 0x3164), (0xE000, 0xF8FF), (0xFE00, 0xFE0F), (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0), (0xFFF0, 0xFFFB), (0x110BD, 0x110BD), (0x110CD, 0x110CD),
    (0x13430, 0x1343F), (0x1BCA0, 0x1BCA3), (0x1D173, 0x1D17A), (0xE0000, 0xE0FFF),
    (0xF0000, 0xFFFFD), (0x100000, 0x10FFFD),
];

/// True for a character a person does not see (see [`HIDDEN`]).
pub fn is_hidden(c: char) -> bool {
    let c = u32::from(c);
    if (0x20..0x7F).contains(&c) {
        return false;
    }
    let at = HIDDEN.partition_point(|&(_, last)| last < c);
    HIDDEN.get(at).is_some_and(|&(first, _)| first <= c)
}

/// Text for messages and JSON: hidden characters as `\u{…}`, `\` as `\\`,
/// at most `max` characters before the cut mark.
pub fn escape(text: &str, max: usize) -> String {
    let (mut out, rest) = escaped(text, max, false);
    push_cut(&mut out, rest);
    out
}

/// [`escape`] in double quotes, with `"` written as `\"`. The quotes do not
/// count towards `max`; the cut mark follows the closing quote.
pub fn quote(text: &str, max: usize) -> String {
    let (body, rest) = escaped(text, max, true);
    let mut out = format!("\"{body}\"");
    push_cut(&mut out, rest);
    out
}

/// The escaped text, cut to `max` characters, and how many characters of
/// the input were left out.
fn escaped(text: &str, max: usize, quotes: bool) -> (String, usize) {
    let mut out = String::with_capacity(text.len().min(max.saturating_add(8)));
    let mut shown = 0usize;
    let mut piece = String::new();
    for (at, c) in text.char_indices() {
        piece.clear();
        if c == '\\' || (quotes && c == '"') {
            piece.push('\\');
            piece.push(c);
        } else if is_hidden(c) {
            let _ = write!(piece, "\\u{{{:x}}}", u32::from(c));
        } else {
            piece.push(c);
        }
        let width = piece.chars().count();
        if shown.saturating_add(width) > max {
            return (out, text[at..].chars().count());
        }
        shown += width;
        out.push_str(&piece);
    }
    (out, 0)
}

fn push_cut(out: &mut String, rest: usize) {
    if rest > 0 {
        let s = if rest == 1 { "" } else { "s" };
        let _ = write!(out, "{CUT}{rest} more character{s}]");
    }
}
