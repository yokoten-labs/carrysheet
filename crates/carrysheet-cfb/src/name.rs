//! Entry names: comparing them as MS-CFB does, checking them, showing
//! them safely.

use std::cmp::Ordering;

use crate::error::CfbError;
use crate::upper_table::UPPER;

/// The longest name, in UTF-16 code units, without the terminating null.
pub(crate) const MAX_NAME_UNITS: usize = 31;

/// One UTF-16 code unit in upper case, as MS-CFB compares names.
///
/// MS-CFB 2.6.4 uses the Unicode simple uppercase mapping, one code unit
/// at a time; its note <3> says Windows Vista and later use Unicode 5.0
/// with a few exceptions. The mapping is pinned to that table (see
/// `upper_table.rs`), so duplicate checks and tree order do not change
/// with the Rust toolchain's Unicode version. Letters added after 5.0,
/// such as Georgian Mtavruli or Cherokee lowercase, stay as they are,
/// and so does half of a surrogate pair.
pub(crate) fn upper(u: u16) -> u16 {
    match UPPER.binary_search_by_key(&u, |&(from, _)| from) {
        Ok(i) => UPPER[i].1,
        Err(_) => u,
    }
}

/// A name in the form names are compared in: each unit upper-cased.
pub(crate) fn key(units: &[u16]) -> Vec<u16> {
    units.iter().map(|&u| upper(u)).collect()
}

/// MS-CFB's order of names in a storage's red-black tree: shorter first,
/// then code unit by code unit in upper case.
pub(crate) fn compare(a: &[u16], b: &[u16]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().map(|&u| upper(u)).cmp(b.iter().map(|&u| upper(u))))
}

/// Whether two names are the same name in a compound file.
pub(crate) fn same(a: &[u16], b: &[u16]) -> bool {
    compare(a, b) == Ordering::Equal
}

/// The UTF-16 units of a name that a new entry may have.
pub(crate) fn checked(name: &str) -> Result<Vec<u16>, CfbError> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let why = if units.is_empty() {
        "it is empty"
    } else if units.len() > MAX_NAME_UNITS {
        "it is longer than 31 UTF-16 code units, the most a directory entry holds"
    } else if name.contains(['/', '\\', ':', '!']) {
        "it contains '/', '\\', ':' or '!', which compound files reserve"
    } else {
        return Ok(units);
    };
    Err(CfbError::InvalidName {
        name: name.to_string(),
        why,
    })
}

/// The names of a path such as `/VBA/dir`; empty parts are skipped, so
/// `/` and `` are the root.
pub(crate) fn split(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

/// Text from a file, safe to put in a message: characters a person does
/// not see are escaped as `\u{XXXX}`, a backslash is doubled so text
/// cannot fake an escape, and long text is cut.
pub(crate) fn show(text: &str) -> String {
    const MAX: usize = 100;
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if i == MAX {
            out.push_str("...");
            break;
        }
        if c == '\\' {
            out.push_str("\\\\");
        } else if hidden(c) {
            out.push_str(&format!("\\u{{{:X}}}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// Characters a person does not see but a program (or a language model)
/// reads: controls, and every character in [`HIDDEN`]. Issue 67 is to
/// share one such helper across the crates.
fn hidden(c: char) -> bool {
    if c.is_control() {
        return true;
    }
    let c = u32::from(c);
    HIDDEN
        .binary_search_by(|&(start, end)| {
            if end < c {
                Ordering::Less
            } else if start > c {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// Sorted, merged ranges of the characters of general category Cf
/// (format: bidi marks, zero-width characters, tags), Co (private use),
/// Zl and Zp (line and paragraph separators), and of the property
/// Default_Ignorable_Code_Point, from Unicode 15.0.
const HIDDEN: [(u32, u32); 28] = [
    (0xAD, 0xAD),
    (0x34F, 0x34F),
    (0x600, 0x605),
    (0x61C, 0x61C),
    (0x6DD, 0x6DD),
    (0x70F, 0x70F),
    (0x890, 0x891),
    (0x8E2, 0x8E2),
    (0x115F, 0x1160),
    (0x17B4, 0x17B5),
    (0x180B, 0x180F),
    (0x200B, 0x200F),
    (0x2028, 0x202E),
    (0x2060, 0x206F),
    (0x3164, 0x3164),
    (0xE000, 0xF8FF),
    (0xFE00, 0xFE0F),
    (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0),
    (0xFFF0, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0000, 0xE0FFF),
    (0xF0000, 0xFFFFD),
    (0x100000, 0x10FFFD),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn order_is_length_then_upper_case() {
        assert_eq!(compare(&u("b"), &u("AA")), Ordering::Less);
        assert_eq!(compare(&u("abc"), &u("ABD")), Ordering::Less);
        assert!(same(&u("Workbook"), &u("WORKBOOK")));
        assert!(same(&u("Módulo"), &u("MÓDULO")));
        // ß upper-cases to two letters; its simple mapping is itself.
        assert!(!same(&u("ß"), &u("S")));
        assert!(same(&u("ß"), &u("ß")));
        // ᾀ's full uppercase is two letters (ἈΙ); its simple uppercase,
        // which MS-CFB uses, is the titlecase ᾈ.
        assert!(same(&u("ᾀ"), &u("ᾈ")));
        assert!(same(&u("ᾳ"), &u("ᾼ")));
        assert!(same(&u("ῳ"), &u("ῼ")));
        assert!(!same(&u("ᾀ"), &u("ἈΙ")));
        // A lone surrogate does not panic.
        assert!(same(&[0xD800], &[0xD800]));
    }

    #[test]
    fn upper_case_is_pinned_to_unicode_5_0() {
        // Ypogegrammeni: Unicode 5.0 simple uppercase (field 12).
        assert_eq!(upper(0x1F80), 0x1F88);
        assert_eq!(upper(0x1F97), 0x1F9F);
        assert_eq!(upper(0x1FA7), 0x1FAF);
        assert_eq!(upper(0x1FB3), 0x1FBC);
        assert_eq!(upper(0x1FF3), 0x1FFC);
        // MS-CFB note <3>, Windows Vista and later: U+1FC3 stays and
        // U+1FCC maps down to it; U+023A and U+023E map to U+2C65 and
        // U+2C66, which stay.
        assert_eq!(upper(0x1FC3), 0x1FC3);
        assert_eq!(upper(0x1FCC), 0x1FC3);
        assert!(same(&u("ῃ"), &u("ῌ")));
        assert_eq!(upper(0x023A), 0x2C65);
        assert_eq!(upper(0x2C65), 0x2C65);
        assert_eq!(upper(0x023E), 0x2C66);
        assert_eq!(upper(0x2C66), 0x2C66);
        assert_eq!(upper(0x03C2), 0x03A3);
        assert_eq!(upper(0x03C3), 0x03A3);
        // Georgian Mkhedruli got capitals (Mtavruli, U+1C90) in Unicode 11
        // and Cherokee got lowercase (U+AB70, U+13F8) in Unicode 8: not in
        // 5.0, so neither changes, whatever the toolchain knows.
        assert_eq!(upper(0x10D0), 0x10D0);
        assert!(!same(&u("ა"), &u("Ა")));
        assert_eq!(upper(0xAB70), 0xAB70);
        assert_eq!(upper(0x13F8), 0x13F8);
        assert!(!same(&u("ꭰ"), &u("Ꭰ")));
        // Above the BMP nothing is a single code unit.
        assert_eq!(upper(0xD801), 0xD801);
        assert!(!same(&u("𐐨"), &u("𐐀")));
    }

    #[test]
    fn upper_table_is_sorted_and_changes_each_unit() {
        let t = &crate::upper_table::UPPER;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(t.iter().all(|&(a, b)| a != b));
    }

    #[test]
    fn names_are_checked() {
        assert!(checked("VBA").is_ok());
        assert!(checked("\u{5}SummaryInformation").is_ok());
        assert!(checked("").is_err());
        assert!(checked("a/b").is_err());
        assert!(checked("x!").is_err());
        assert!(checked(&"n".repeat(31)).is_ok());
        assert!(checked(&"n".repeat(32)).is_err());
    }

    #[test]
    fn shown_text_is_escaped_and_cut() {
        assert_eq!(show("\u{5}Doc"), "\\u{5}Doc");
        // Bidi overrides, separators, zero-width and tag characters.
        assert_eq!(
            show("a\u{202E}b\u{2028}c\u{200B}d\u{E0041}"),
            "a\\u{202E}b\\u{2028}c\\u{200B}d\\u{E0041}"
        );
        // A backslash cannot fake an escape.
        assert_eq!(show("\\u{5}"), "\\\\u{5}");
        assert_eq!(show(&"x".repeat(150)).len(), 103);
    }
}
