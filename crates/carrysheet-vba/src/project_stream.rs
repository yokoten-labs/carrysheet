//! The `PROJECT` stream, MS-OVBA 2.3.1: a text file of `key=value` lines
//! that lists the modules the editor shows, and the protection state.

use crate::{ModuleKind, PasswordState, Protection};

/// What the `PROJECT` stream says.
#[derive(Debug, Default)]
pub(crate) struct ProjectStream {
    /// Module names with the kind the stream gives them, in stream order.
    pub modules: Vec<(String, ModuleKind)>,
    pub protection: Protection,
}

/// Lines read from the stream at most; real ones have a few dozen.
const MAX_LINES: usize = 100_000;

/// Parses the decoded text of a `PROJECT` stream.
pub(crate) fn parse(text: &str, max_modules: usize) -> ProjectStream {
    let mut out = ProjectStream::default();
    for line in crate::source_lines(text).take(MAX_LINES) {
        // Sections such as [Host Extender Info] and [Workspace] follow the
        // properties and hold nothing this reader needs.
        if line.starts_with('[') {
            break;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let kind = if key.eq_ignore_ascii_case("Document") {
            Some(ModuleKind::Document)
        } else if key.eq_ignore_ascii_case("Module") {
            Some(ModuleKind::Standard)
        } else if key.eq_ignore_ascii_case("Class") {
            Some(ModuleKind::Class)
        } else if key.eq_ignore_ascii_case("BaseClass") {
            Some(ModuleKind::Form)
        } else {
            None
        };
        if let Some(kind) = kind {
            // Document=Sheet1/&H00000000: the name ends at the last slash.
            let name = if kind == ModuleKind::Document {
                value.rsplit_once('/').map_or(value, |(n, _)| n)
            } else {
                value
            };
            if out.modules.len() < max_modules {
                out.modules.push((name.to_string(), kind));
            }
            continue;
        }
        let p = &mut out.protection;
        if key.eq_ignore_ascii_case("CMG") {
            p.cmg_present = true;
            if let Some(data) = decrypt_hex(unquote(value))
                && let [a, b, c, d] = data[..]
            {
                let state = u32::from_le_bytes([a, b, c, d]);
                p.user_protected = Some(state & 1 != 0);
                p.host_protected = Some(state & 2 != 0);
                p.vbe_protected = Some(state & 4 != 0);
            }
        } else if key.eq_ignore_ascii_case("DPB") {
            p.dpb_present = true;
            p.password = match decrypt_hex(unquote(value)) {
                Some(data) if data == [0] => PasswordState::None,
                // Hash Data Structure (2.4.4.1): 29 bytes starting with a
                // reserved 0xFF. A 28-character plain password plus its null
                // is also 29 bytes, so the length alone is not enough.
                Some(data) if data.len() == 29 && data[0] == 0xFF => PasswordState::Hash,
                // A plain-text password: never keep or show its value.
                Some(data) if data.last() == Some(&0) => PasswordState::PlainText,
                _ => PasswordState::Unreadable,
            };
        } else if key.eq_ignore_ascii_case("GC") {
            p.gc_present = true;
            if let Some(data) = decrypt_hex(unquote(value)) {
                match data.as_slice() {
                    [0x00] => p.visible = Some(false),
                    [0xFF] => p.visible = Some(true),
                    _ => {}
                }
            }
        }
    }
    out
}

fn unquote(value: &str) -> &str {
    let v = value.trim();
    v.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(v)
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Undoes the MS-OVBA Data Encryption (2.4.3.3) of a CMG, DPB or GC value.
///
/// This is obfuscation with a key stored next to the data, not real
/// encryption. Returns `None` when the value is malformed.
pub(crate) fn decrypt_hex(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.as_bytes();
    // The longest valid value (DPB with a 29-byte hash) is 72 hex digits.
    if !hex.len().is_multiple_of(2) || hex.len() > 1024 {
        return None;
    }
    let bytes: Vec<u8> = hex
        .chunks(2)
        .map(|p| Some(hex_value(p[0])? << 4 | hex_value(p[1])?))
        .collect::<Option<_>>()?;
    let (&seed, rest) = bytes.split_first()?;
    let (&version_enc, rest) = rest.split_first()?;
    let (&proj_key_enc, rest) = rest.split_first()?;
    if seed ^ version_enc != 2 {
        return None;
    }
    let mut unencrypted1 = seed ^ proj_key_enc;
    let mut encrypted1 = proj_key_enc;
    let mut encrypted2 = version_enc;
    let mut step = |byte_enc: u8| {
        let byte = byte_enc ^ encrypted2.wrapping_add(unencrypted1);
        encrypted2 = encrypted1;
        encrypted1 = byte_enc;
        unencrypted1 = byte;
        byte
    };
    let ignored = usize::from((seed & 6) / 2);
    let mut it = rest.iter().copied();
    for _ in 0..ignored {
        step(it.next()?);
    }
    let mut length: u32 = 0;
    for i in 0..4 {
        length |= u32::from(step(it.next()?)) << (8 * i);
    }
    let data: Vec<u8> = it.map(&mut step).collect();
    // The data must be exactly as long as declared.
    (data.len() == length as usize).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the MS-OVBA 2.4.3.2 encryption, to test decryption against it.
    fn encrypt(seed: u8, proj_key: u8, data: &[u8]) -> String {
        let version_enc = seed ^ 2;
        let proj_key_enc = seed ^ proj_key;
        let mut out = vec![seed, version_enc, proj_key_enc];
        let mut unencrypted1 = proj_key;
        let mut encrypted1 = proj_key_enc;
        let mut encrypted2 = version_enc;
        let mut push = |byte: u8, out: &mut Vec<u8>| {
            let byte_enc = byte ^ encrypted2.wrapping_add(unencrypted1);
            out.push(byte_enc);
            encrypted2 = encrypted1;
            encrypted1 = byte_enc;
            unencrypted1 = byte;
        };
        let ignored = usize::from((seed & 6) / 2);
        for _ in 0..ignored {
            push(0x07, &mut out);
        }
        for b in (data.len() as u32).to_le_bytes() {
            push(b, &mut out);
        }
        for &b in data {
            push(b, &mut out);
        }
        out.iter().map(|b| format!("{b:02X}")).collect()
    }

    #[test]
    fn decryption_inverts_encryption() {
        for seed in [0x00, 0x02, 0x06, 0x7F, 0xFE] {
            for data in [&[0xFFu8][..], &[0, 0, 0, 0], &[1, 2, 3, 4, 5]] {
                let hex = encrypt(seed, 0x5A, data);
                assert_eq!(decrypt_hex(&hex).as_deref(), Some(data), "seed {seed:#x}");
            }
        }
    }

    #[test]
    fn a_28_character_password_is_plain_text_not_a_hash() {
        let mut plain = b"abcdefghijklmnopqrstuvwxyz12".to_vec();
        plain.push(0);
        assert_eq!(plain.len(), 29);
        let mut hash = vec![0xFF];
        hash.extend_from_slice(&[0x11; 28]);
        for (data, want) in [
            (plain, PasswordState::PlainText),
            (hash, PasswordState::Hash),
        ] {
            let text = format!("DPB=\"{}\"\r\n", encrypt(0x14, 0x33, &data));
            assert_eq!(parse(&text, 10).protection.password, want);
        }
    }

    #[test]
    fn malformed_values_are_none() {
        assert_eq!(decrypt_hex(""), None);
        assert_eq!(decrypt_hex("0"), None);
        assert_eq!(decrypt_hex("ZZ0000"), None);
        // Version byte not 2.
        assert_eq!(decrypt_hex("000000000000000000"), None);
    }

    #[test]
    fn reads_modules_and_protection() {
        let text = format!(
            "ID=\"{{00000000-0000-0000-0000-000000000000}}\"\r\nDocument=ThisWorkbook/&H00000000\r\n\
             Module=Module1\r\nClass=Class1\r\nBaseClass=UserForm1\r\nName=\"VBAProject\"\r\n\
             CMG=\"{}\"\r\nDPB=\"{}\"\r\nGC=\"{}\"\r\n\r\n[Host Extender Info]\r\nModule=Ghost\r\n",
            encrypt(0x10, 0x33, &4u32.to_le_bytes()),
            encrypt(0x22, 0x33, &[0]),
            encrypt(0x08, 0x33, &[0]),
        );
        let p = parse(&text, 100);
        assert_eq!(
            p.modules,
            [
                ("ThisWorkbook".to_string(), ModuleKind::Document),
                ("Module1".to_string(), ModuleKind::Standard),
                ("Class1".to_string(), ModuleKind::Class),
                ("UserForm1".to_string(), ModuleKind::Form),
            ]
        );
        let pr = p.protection;
        assert!(pr.cmg_present && pr.dpb_present && pr.gc_present);
        assert_eq!(pr.vbe_protected, Some(true));
        assert_eq!(pr.user_protected, Some(false));
        assert_eq!(pr.password, PasswordState::None);
        assert_eq!(pr.visible, Some(false));
        assert!(pr.locked_for_viewing());
    }
}
