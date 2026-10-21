//! Small building blocks: a bounds-checked byte reader, base64, the hash
//! and AES wrappers, random bytes and compound file errors.

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use sha1::Digest;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

use crate::error::CryptoError;

/// Reads little-endian numbers from a byte slice without ever panicking.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    part: &'static str,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8], part: &'static str) -> Self {
        Reader {
            bytes,
            pos: 0,
            part,
        }
    }

    pub(crate) fn rest(&self) -> &'a [u8] {
        self.bytes.get(self.pos..).unwrap_or(&[])
    }

    pub(crate) fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8], CryptoError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.bytes.len());
        match end {
            Some(end) => {
                let out = &self.bytes[self.pos..end];
                self.pos = end;
                Ok(out)
            }
            None => Err(CryptoError::malformed(
                self.part,
                format!(
                    "it ends at byte {} while reading {what} ({n} bytes needed at byte {})",
                    self.bytes.len(),
                    self.pos
                ),
            )),
        }
    }

    pub(crate) fn u16(&mut self, what: &str) -> Result<u16, CryptoError> {
        let b = self.take(2, what)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self, what: &str) -> Result<u32, CryptoError> {
        let b = self.take(4, what)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64(&mut self, what: &str) -> Result<u64, CryptoError> {
        let b = self.take(8, what)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
}

/// Compares two secrets in constant time (for verifiers and HMACs).
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Fills a buffer with random bytes from the operating system (or, on
/// `wasm32-unknown-unknown`, the browser).
pub(crate) fn random(n: usize) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let mut buf = Zeroizing::new(vec![0u8; n]);
    getrandom::fill(&mut buf).map_err(|e| CryptoError::Random {
        detail: e.to_string(),
    })?;
    Ok(buf)
}

pub(crate) fn container_error(e: carrysheet_cfb::CfbError) -> CryptoError {
    CryptoError::Container {
        detail: e.to_string(),
    }
}

/// Text from a file, made safe to show: printable ASCII only (anything
/// else becomes `?`), at most `max` characters.
pub(crate) fn clean(text: &str, max: usize) -> String {
    let mut out: String = text
        .chars()
        .take(max)
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .collect();
    if text.chars().nth(max).is_some() {
        out.push_str("...");
    }
    out
}

/// A name from a file: one of the `known` spellings as is, or [`clean`]ed
/// and cut to 32 characters.
pub(crate) fn known_or_clean(text: &str, known: &[&'static str]) -> String {
    match known.iter().find(|k| **k == text) {
        Some(k) => (*k).to_string(),
        None => clean(text, 32),
    }
}

// ---------------------------------------------------------------- base64

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, as in the agile `EncryptionInfo` XML.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(B64[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes standard base64; ASCII whitespace is skipped. `max` bounds the
/// decoded length, so a huge attribute cannot make a huge buffer.
pub(crate) fn base64_decode(
    text: &str,
    max: usize,
    what: &'static str,
) -> Result<Vec<u8>, CryptoError> {
    let bad = |detail: String| CryptoError::malformed("EncryptionInfo", detail);
    let mut out = Vec::with_capacity((text.len() / 4 * 3).min(max));
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut padding = 0usize;
    for c in text.bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return Err(bad(format!("{what} has data after its base64 padding")));
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(bad(format!("{what} is not valid base64"))),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if out.len() == max {
                return Err(bad(format!("{what} is longer than {max} bytes")));
            }
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if padding > 2 {
        return Err(bad(format!("{what} has too much base64 padding")));
    }
    Ok(out)
}

// ---------------------------------------------------------------- hashes

/// A hash algorithm named in the encryption data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HashAlgorithm {
    /// MD5 (only inside RC4 encryption of `.xls`).
    Md5,
    /// SHA-1.
    Sha1,
    /// SHA-256.
    Sha256,
    /// SHA-384.
    Sha384,
    /// SHA-512.
    Sha512,
}

impl HashAlgorithm {
    /// The name used in agile `EncryptionInfo` XML, such as `SHA512`.
    pub fn name(self) -> &'static str {
        match self {
            HashAlgorithm::Md5 => "MD5",
            HashAlgorithm::Sha1 => "SHA1",
            HashAlgorithm::Sha256 => "SHA256",
            HashAlgorithm::Sha384 => "SHA384",
            HashAlgorithm::Sha512 => "SHA512",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        // MS-OFFCRYPTO 2.3.4.10 spells SHA-1 as "SHA-1"; some writers use "SHA1".
        match name {
            "MD5" => Some(HashAlgorithm::Md5),
            "SHA1" | "SHA-1" => Some(HashAlgorithm::Sha1),
            "SHA256" => Some(HashAlgorithm::Sha256),
            "SHA384" => Some(HashAlgorithm::Sha384),
            "SHA512" => Some(HashAlgorithm::Sha512),
            _ => None,
        }
    }

    /// The length of a digest, in bytes.
    pub fn size(self) -> usize {
        match self {
            HashAlgorithm::Md5 => 16,
            HashAlgorithm::Sha1 => 20,
            HashAlgorithm::Sha256 => 32,
            HashAlgorithm::Sha384 => 48,
            HashAlgorithm::Sha512 => 64,
        }
    }

    /// The digest of the parts joined together.
    pub(crate) fn digest(self, parts: &[&[u8]]) -> Zeroizing<Vec<u8>> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Zeroizing<Vec<u8>> {
            let mut h = D::new();
            for p in parts {
                h.update(p);
            }
            let mut out = h.finalize();
            let v = Zeroizing::new(out.to_vec());
            out.as_mut_slice().zeroize();
            v
        }
        match self {
            HashAlgorithm::Md5 => run::<md5::Md5>(parts),
            HashAlgorithm::Sha1 => run::<sha1::Sha1>(parts),
            HashAlgorithm::Sha256 => run::<sha2::Sha256>(parts),
            HashAlgorithm::Sha384 => run::<sha2::Sha384>(parts),
            HashAlgorithm::Sha512 => run::<sha2::Sha512>(parts),
        }
    }

    /// MS-OFFCRYPTO 2.3.4.7 / 2.3.4.11: H0 = H(salt + password), then
    /// Hn = H(iterator + Hn-1) for `spin` rounds.
    pub(crate) fn spin(self, salt: &[u8], password: &[u8], spin: u32) -> Zeroizing<Vec<u8>> {
        fn run<D: Digest>(salt: &[u8], password: &[u8], spin: u32) -> Zeroizing<Vec<u8>> {
            let mut h = D::new_with_prefix(salt);
            h.update(password);
            let mut cur = h.finalize();
            for i in 0..spin {
                let mut h = D::new_with_prefix(i.to_le_bytes());
                h.update(&cur);
                let mut next = h.finalize();
                std::mem::swap(&mut cur, &mut next);
                next.as_mut_slice().zeroize();
            }
            let v = Zeroizing::new(cur.to_vec());
            cur.as_mut_slice().zeroize();
            v
        }
        match self {
            HashAlgorithm::Md5 => run::<md5::Md5>(salt, password, spin),
            HashAlgorithm::Sha1 => run::<sha1::Sha1>(salt, password, spin),
            HashAlgorithm::Sha256 => run::<sha2::Sha256>(salt, password, spin),
            HashAlgorithm::Sha384 => run::<sha2::Sha384>(salt, password, spin),
            HashAlgorithm::Sha512 => run::<sha2::Sha512>(salt, password, spin),
        }
    }

    /// HMAC of `data` under `key`.
    pub(crate) fn hmac(self, key: &[u8], data: &[u8]) -> Zeroizing<Vec<u8>> {
        fn run<M: Mac + KeyInit>(key: &[u8], data: &[u8]) -> Zeroizing<Vec<u8>> {
            // HMAC takes keys of any length, so this never fails.
            let Ok(mut m) = <M as KeyInit>::new_from_slice(key) else {
                return Zeroizing::new(Vec::new());
            };
            m.update(data);
            Zeroizing::new(m.finalize().into_bytes().to_vec())
        }
        match self {
            HashAlgorithm::Md5 => run::<Hmac<md5::Md5>>(key, data),
            HashAlgorithm::Sha1 => run::<Hmac<sha1::Sha1>>(key, data),
            HashAlgorithm::Sha256 => run::<Hmac<sha2::Sha256>>(key, data),
            HashAlgorithm::Sha384 => run::<Hmac<sha2::Sha384>>(key, data),
            HashAlgorithm::Sha512 => run::<Hmac<sha2::Sha512>>(key, data),
        }
    }
}

/// Cuts `bytes` to `len`, or pads it to `len` with `pad`
/// (MS-OFFCRYPTO 2.3.4.12, used for agile keys and IVs).
pub(crate) fn fit(bytes: &[u8], len: usize, pad: u8) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(vec![pad; len]);
    let n = len.min(bytes.len());
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

// ---------------------------------------------------------------- AES

/// AES with a 128, 192 or 256-bit key, chosen at run time.
///
/// CBC and ECB are written out here over the block cipher: they are a few
/// lines each, and doing them here avoids a generic type per key size.
pub(crate) enum Aes {
    A128(aes::Aes128),
    A192(aes::Aes192),
    A256(aes::Aes256),
}

pub(crate) const AES_BLOCK: usize = 16;

impl Aes {
    pub(crate) fn new(key: &[u8]) -> Result<Aes, CryptoError> {
        let bad =
            || CryptoError::malformed("EncryptionInfo", "the AES key is not 16, 24 or 32 bytes");
        Ok(match key.len() {
            16 => Aes::A128(aes::Aes128::new_from_slice(key).map_err(|_| bad())?),
            24 => Aes::A192(aes::Aes192::new_from_slice(key).map_err(|_| bad())?),
            32 => Aes::A256(aes::Aes256::new_from_slice(key).map_err(|_| bad())?),
            _ => return Err(bad()),
        })
    }

    fn decrypt_block(&self, block: &mut [u8]) {
        let mut b = aes::Block::default();
        b.copy_from_slice(block);
        match self {
            Aes::A128(c) => c.decrypt_block(&mut b),
            Aes::A192(c) => c.decrypt_block(&mut b),
            Aes::A256(c) => c.decrypt_block(&mut b),
        }
        block.copy_from_slice(&b);
        b.as_mut_slice().zeroize();
    }

    fn encrypt_block(&self, block: &mut [u8]) {
        let mut b = aes::Block::default();
        b.copy_from_slice(block);
        match self {
            Aes::A128(c) => c.encrypt_block(&mut b),
            Aes::A192(c) => c.encrypt_block(&mut b),
            Aes::A256(c) => c.encrypt_block(&mut b),
        }
        block.copy_from_slice(&b);
    }

    /// ECB decryption in place; a trailing partial block is left as is.
    pub(crate) fn ecb_decrypt(&self, data: &mut [u8]) {
        for block in data.as_chunks_mut::<AES_BLOCK>().0 {
            self.decrypt_block(block);
        }
    }

    /// CBC decryption in place; a trailing partial block is left as is.
    pub(crate) fn cbc_decrypt(&self, iv: &[u8], data: &mut [u8]) {
        let mut prev = [0u8; AES_BLOCK];
        prev.copy_from_slice(&iv[..AES_BLOCK]);
        for block in data.as_chunks_mut::<AES_BLOCK>().0 {
            let mut cipher = [0u8; AES_BLOCK];
            cipher.copy_from_slice(block);
            self.decrypt_block(block);
            for (b, p) in block.iter_mut().zip(prev.iter()) {
                *b ^= p;
            }
            prev = cipher;
        }
    }

    /// CBC encryption in place; `data` must be whole blocks.
    pub(crate) fn cbc_encrypt(&self, iv: &[u8], data: &mut [u8]) {
        let mut prev = [0u8; AES_BLOCK];
        prev.copy_from_slice(&iv[..AES_BLOCK]);
        for block in data.as_chunks_mut::<AES_BLOCK>().0 {
            for (b, p) in block.iter_mut().zip(prev.iter()) {
                *b ^= p;
            }
            self.encrypt_block(block);
            prev.copy_from_slice(block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for n in 0..40usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let text = base64_encode(&data);
            assert_eq!(base64_decode(&text, 1000, "x").unwrap(), data);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
    }

    #[test]
    fn base64_rejects_garbage_and_huge() {
        assert!(base64_decode("TW!u", 10, "x").is_err());
        assert!(base64_decode("TQ==TQ==", 10, "x").is_err());
        assert!(base64_decode(&"A".repeat(4000), 16, "x").is_err());
    }

    #[test]
    fn aes_cbc_matches_nist_sp800_38a() {
        // NIST SP 800-38A F.2.1, CBC-AES128.Encrypt, first block.
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex("000102030405060708090a0b0c0d0e0f");
        let mut data = hex("6bc1bee22e409f96e93d7e117393172a");
        let aes = Aes::new(&key).unwrap();
        aes.cbc_encrypt(&iv, &mut data);
        assert_eq!(data, hex("7649abac8119b246cee98e9b12e9197d"));
        aes.cbc_decrypt(&iv, &mut data);
        assert_eq!(data, hex("6bc1bee22e409f96e93d7e117393172a"));
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
