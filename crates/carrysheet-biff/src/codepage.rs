//! Code pages for the byte strings of BIFF2 to BIFF5 (CODEPAGE record).

use encoding_rs::Encoding;

/// The encoding for a CODEPAGE value, or `None` if we cannot decode it.
///
/// BIFF8 stores text as UTF-16 (code page 1200); its "compressed" strings are
/// Latin-1 and never go through this table.
pub(crate) fn encoding_for(codepage: u16) -> Option<&'static Encoding> {
    use encoding_rs::*;
    Some(match codepage {
        // 367 is US-ASCII; 20127 too. windows-1252 is a superset.
        367 | 20127 | 1252 | 32769 | 28591 => WINDOWS_1252,
        // 32768 is how BIFF2 and BIFF3 files from the Mac say "Mac Roman".
        10000 | 32768 => MACINTOSH,
        874 => WINDOWS_874,
        932 | 10001 => SHIFT_JIS,
        936 | 10008 => GBK,
        949 | 10003 | 51949 => EUC_KR,
        950 | 10002 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        866 => IBM866,
        10007 => X_MAC_CYRILLIC,
        20866 => KOI8_R,
        21866 => KOI8_U,
        28592 => ISO_8859_2,
        28593 => ISO_8859_3,
        28594 => ISO_8859_4,
        28595 => ISO_8859_5,
        28596 => ISO_8859_6,
        28597 => ISO_8859_7,
        28598 => ISO_8859_8,
        28599 => WINDOWS_1254,
        54936 => GB18030,
        65001 => UTF_8,
        _ => return None,
    })
}

/// Decodes `bytes` with `enc`, replacing invalid sequences with U+FFFD.
pub(crate) fn decode(enc: &'static Encoding, bytes: &[u8]) -> String {
    // Old Excel never writes a byte order mark, so do not sniff one.
    let (text, _) = enc.decode_without_bom_handling(bytes);
    text.into_owned()
}

/// Decodes BIFF8 "compressed" characters: each byte is a code point U+0000 to U+00FF.
pub(crate) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Decodes UTF-16LE bytes (an even count), replacing unpaired surrogates.
pub(crate) fn utf16le(bytes: &[u8]) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c));
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_cjk_and_western_pages() {
        // "中文" in GBK.
        assert_eq!(
            decode(encoding_for(936).unwrap(), &[0xD6, 0xD0, 0xCE, 0xC4]),
            "中文"
        );
        // "中文" in Big5.
        assert_eq!(
            decode(encoding_for(950).unwrap(), &[0xA4, 0xA4, 0xA4, 0xE5]),
            "中文"
        );
        // "日本" in Shift-JIS.
        assert_eq!(
            decode(encoding_for(932).unwrap(), &[0x93, 0xFA, 0x96, 0x7B]),
            "日本"
        );
        // "한" in code page 949.
        assert_eq!(decode(encoding_for(949).unwrap(), &[0xC7, 0xD1]), "한");
        // Euro sign in windows-1252, e-acute in Mac Roman.
        assert_eq!(decode(encoding_for(1252).unwrap(), &[0x80]), "€");
        assert_eq!(decode(encoding_for(32768).unwrap(), &[0x8E]), "é");
        for cp in 1250..=1258 {
            assert!(encoding_for(cp).is_some(), "code page {cp}");
        }
        assert!(encoding_for(437).is_none());
    }

    #[test]
    fn latin1_is_not_windows_1252() {
        assert_eq!(latin1(&[0x41, 0x80, 0xE9]), "A\u{80}é");
    }
}
