//! XOR obfuscation, method 1 (MS-OFFCRYPTO 2.3.7.1 - 2.3.7.3; MS-XLS
//! 2.2.10). Weak by design: it only hides the bytes.
//!
//! The 16-byte XOR array follows Apache POI's `CryptoFunctions` (Apache
//! License 2.0): Excel rotates each array byte left by 2 bits, a detail
//! the spec's pseudocode leaves out.

use zeroize::Zeroizing;

use crate::Password;
use crate::error::CryptoError;

/// The longest password XOR obfuscation can hold.
pub(crate) const MAX_XOR_PASSWORD: usize = 15;

const INITIAL_CODE: [u16; 15] = [
    0xE1F0, 0x1D0F, 0xCC9C, 0x84C0, 0x110C, 0x0E10, 0xF1CE, 0x313E, 0x1872, 0xE139, 0xD40F, 0x84F9,
    0x280C, 0xA96A, 0x4EC3,
];

const PAD: [u8; 15] = [
    0xBB, 0xFF, 0xFF, 0xBA, 0xFF, 0xFF, 0xB9, 0x80, 0x00, 0xBE, 0x0F, 0x00, 0xBF, 0x0F, 0x00,
];

const MATRIX: [[u16; 7]; 15] = [
    [0xAEFC, 0x4DD9, 0x9BB2, 0x2745, 0x4E8A, 0x9D14, 0x2A09],
    [0x7B61, 0xF6C2, 0xFDA5, 0xEB6B, 0xC6F7, 0x9DCF, 0x2BBF],
    [0x4563, 0x8AC6, 0x05AD, 0x0B5A, 0x16B4, 0x2D68, 0x5AD0],
    [0x0375, 0x06EA, 0x0DD4, 0x1BA8, 0x3750, 0x6EA0, 0xDD40],
    [0xD849, 0xA0B3, 0x5147, 0xA28E, 0x553D, 0xAA7A, 0x44D5],
    [0x6F45, 0xDE8A, 0xAD35, 0x4A4B, 0x9496, 0x390D, 0x721A],
    [0xEB23, 0xC667, 0x9CEF, 0x29FF, 0x53FE, 0xA7FC, 0x5FD9],
    [0x47D3, 0x8FA6, 0x0F6D, 0x1EDA, 0x3DB4, 0x7B68, 0xF6D0],
    [0xB861, 0x60E3, 0xC1C6, 0x93AD, 0x377B, 0x6EF6, 0xDDEC],
    [0x45A0, 0x8B40, 0x06A1, 0x0D42, 0x1A84, 0x3508, 0x6A10],
    [0xAA51, 0x4483, 0x8906, 0x022D, 0x045A, 0x08B4, 0x1168],
    [0x76B4, 0xED68, 0xCAF1, 0x85C3, 0x1BA7, 0x374E, 0x6E9C],
    [0x3730, 0x6E60, 0xDCC0, 0xA9A1, 0x4363, 0x86C6, 0x1DAD],
    [0x3331, 0x6662, 0xCCC4, 0x89A9, 0x0373, 0x06E6, 0x0DCC],
    [0x1021, 0x2042, 0x4084, 0x8108, 0x1231, 0x2462, 0x48C4],
];

/// The password as single bytes: each character's low byte, or its high
/// byte when the low byte is zero (2.3.7.1).
fn ansi(password: &Password) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    // Count at most 16 units before allocating anything.
    if password
        .as_str()
        .encode_utf16()
        .take(MAX_XOR_PASSWORD + 1)
        .count()
        > MAX_XOR_PASSWORD
    {
        return Err(CryptoError::PasswordTooLong {
            max: MAX_XOR_PASSWORD,
        });
    }
    let mut out = Zeroizing::new(Vec::with_capacity(MAX_XOR_PASSWORD));
    for u in password.as_str().encode_utf16() {
        let [lo, hi] = u.to_le_bytes();
        out.push(if lo != 0 { lo } else { hi });
    }
    Ok(out)
}

/// The key (2.3.7.2 CreateXorKey_Method1).
fn key(bytes: &[u8]) -> u16 {
    let Some(&initial) = bytes.len().checked_sub(1).and_then(|i| INITIAL_CODE.get(i)) else {
        return 0;
    };
    let mut key = initial;
    let first_line = MAX_XOR_PASSWORD - bytes.len();
    for (line, &b) in (first_line..).zip(bytes) {
        let mut ch = b;
        for &m in &MATRIX[line] {
            if ch & 1 == 1 {
                key ^= m;
            }
            ch >>= 1;
        }
    }
    key
}

/// The verifier (2.3.7.1 CreatePasswordVerifier_Method1).
fn verifier(bytes: &[u8]) -> u16 {
    if bytes.is_empty() {
        return 0;
    }
    let rotate = |v: u16| ((v << 1) & 0x7FFF) | ((v >> 14) & 1);
    let mut v: u16 = 0;
    for &b in bytes.iter().rev() {
        v = rotate(v) ^ u16::from(b);
    }
    v = rotate(v) ^ bytes.len() as u16;
    v ^ 0xCE4B
}

/// The 16-byte XOR array for Excel.
fn array(bytes: &[u8], key: u16) -> Zeroizing<[u8; 16]> {
    let mut out = Zeroizing::new([0u8; 16]);
    out[..bytes.len()].copy_from_slice(bytes);
    let pad_len = if bytes.is_empty() {
        15
    } else {
        16 - bytes.len()
    };
    out[bytes.len()..bytes.len() + pad_len].copy_from_slice(&PAD[..pad_len]);
    let k = key.to_le_bytes();
    for (i, b) in out.iter_mut().enumerate() {
        *b = (*b ^ k[i & 1]).rotate_left(2);
    }
    out
}

/// Checks a password against the FILEPASS key and verifier, and returns
/// the XOR array.
pub(crate) fn open(
    password: &Password,
    file_key: u16,
    file_verifier: u16,
) -> Result<Zeroizing<[u8; 16]>, CryptoError> {
    let bytes = ansi(password)?;
    let k = key(&bytes);
    if k != file_key || verifier(&bytes) != file_verifier {
        return Err(CryptoError::WrongPassword);
    }
    Ok(array(&bytes, k))
}

/// Decrypts one byte at stream position `pos` of a record whose body is
/// `record_len` bytes long.
pub(crate) fn decrypt_byte(b: u8, array: &[u8; 16], pos: usize, record_len: usize) -> u8 {
    b.rotate_left(3) ^ array[pos.wrapping_add(record_len) & 15]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values from Apache POI's TestXorEncryption.
    #[test]
    fn abc_matches_poi() {
        let b = ansi(&Password::new("abc")).unwrap();
        assert_eq!(key(&b), 20810);
        assert_eq!(verifier(&b), 52250);
        let expected = [
            0xAC, 0xCC, 0xA4, 0xAB, 0xD6, 0xBA, 0xC3, 0xBA, 0xD6, 0xA3, 0x2B, 0x45, 0xD3, 0x79,
            0x29, 0xBB,
        ];
        assert_eq!(*array(&b, key(&b)), expected);
    }

    #[test]
    fn empty_matches_poi() {
        let b = ansi(&Password::new("")).unwrap();
        assert_eq!(key(&b), 0);
        assert_eq!(verifier(&b), 0);
        let expected = [
            0xEE, 0xFF, 0xFF, 0xEA, 0xFF, 0xFF, 0xE6, 0x02, 0x00, 0xFA, 0x3C, 0x00, 0xFE, 0x3C,
            0x00, 0x00,
        ];
        assert_eq!(*array(&b, key(&b)), expected);
    }
}
