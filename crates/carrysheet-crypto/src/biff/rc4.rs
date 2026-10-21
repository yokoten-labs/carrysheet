//! RC4 keys for `.xls`: the MD5-based method (MS-OFFCRYPTO 2.3.6) and
//! CryptoAPI (2.3.5), with a new key every 1024 bytes of the stream.

use rc4::{KeyInit, Rc4, StreamCipher};
use zeroize::Zeroizing;

use crate::error::CryptoError;
use crate::util::{HashAlgorithm, ct_eq};

/// The stream is rekeyed every this many bytes (MS-XLS 2.2.10).
pub(crate) const REKEY: usize = 1024;

/// What a block key is made from, once the password is known.
pub(crate) enum KeyBase {
    /// RC4 (MD5): the first 5 bytes of MD5(16 × (MD5(pw)[..5] + salt)).
    Md5(Zeroizing<Vec<u8>>),
    /// CryptoAPI: H0 = SHA-1(salt + pw), and the key size in bytes.
    CryptoApi(Zeroizing<Vec<u8>>, usize),
}

impl KeyBase {
    pub(crate) fn md5(password_utf16: &[u8], salt: &[u8]) -> KeyBase {
        let h0 = HashAlgorithm::Md5.digest(&[password_utf16]);
        let mut buf = Zeroizing::new(Vec::with_capacity(16 * 21));
        for _ in 0..16 {
            buf.extend_from_slice(&h0[..5]);
            buf.extend_from_slice(salt);
        }
        let h1 = HashAlgorithm::Md5.digest(&[&buf]);
        KeyBase::Md5(Zeroizing::new(h1[..5].to_vec()))
    }

    pub(crate) fn cryptoapi(password_utf16: &[u8], salt: &[u8], key_bits: u32) -> KeyBase {
        let h0 = HashAlgorithm::Sha1.digest(&[salt, password_utf16]);
        KeyBase::CryptoApi(h0, key_bits as usize / 8)
    }

    /// The RC4 cipher for one 1024-byte block of the stream.
    pub(crate) fn cipher(&self, block: u32) -> Result<Rc4, CryptoError> {
        let b = block.to_le_bytes();
        let key = match self {
            KeyBase::Md5(base) => HashAlgorithm::Md5.digest(&[base, &b]),
            KeyBase::CryptoApi(h0, len) => {
                let h = HashAlgorithm::Sha1.digest(&[h0, &b]);
                // A 40-bit key is used as 128 bits, padded with zeros
                // (2.3.5.2); other sizes are used as they are.
                let mut key = Zeroizing::new(h[..*len].to_vec());
                if *len == 5 {
                    key.resize(16, 0);
                }
                key
            }
        };
        // Keys are 5 to 20 bytes, inside RC4's 1 to 256.
        Rc4::new_from_slice(&key).map_err(|_| {
            CryptoError::malformed("FILEPASS record", "the RC4 key size is out of range")
        })
    }

    /// Checks the verifier: decrypting it and its hash with the block 0
    /// key, one RC4 stream, must give a matching hash.
    pub(crate) fn verify(&self, verifier: &[u8], verifier_hash: &[u8]) -> Result<(), CryptoError> {
        let mut c = self.cipher(0)?;
        let mut v = Zeroizing::new(verifier.to_vec());
        let mut h = Zeroizing::new(verifier_hash.to_vec());
        c.apply_keystream(&mut v);
        c.apply_keystream(&mut h);
        let (alg, len) = match self {
            KeyBase::Md5(_) => (HashAlgorithm::Md5, 16),
            KeyBase::CryptoApi(..) => (HashAlgorithm::Sha1, 20),
        };
        if h.len() < len || !ct_eq(&alg.digest(&[&v]), &h[..len]) {
            return Err(CryptoError::WrongPassword);
        }
        Ok(())
    }
}

/// Applies the keystream by stream position: the byte at position `p`
/// uses keystream byte `p % 1024` of block `p / 1024`, whether or not the
/// bytes before it were encrypted.
pub(crate) struct Keystream<'a> {
    base: &'a KeyBase,
    block: Option<u32>,
    cipher: Option<Rc4>,
    at: usize,
    /// Where skipped keystream goes; wiped when the keystream is dropped.
    scratch: Zeroizing<[u8; REKEY]>,
}

impl<'a> Keystream<'a> {
    pub(crate) fn new(base: &'a KeyBase) -> Self {
        Keystream {
            base,
            block: None,
            cipher: None,
            at: 0,
            scratch: Zeroizing::new([0u8; REKEY]),
        }
    }

    /// XORs `buf`, which sits at stream position `pos`, with the keystream.
    /// An empty `buf` costs nothing (empty records are common).
    pub(crate) fn apply(&mut self, pos: usize, buf: &mut [u8]) -> Result<(), CryptoError> {
        if buf.is_empty() {
            return Ok(());
        }
        let mut pos = pos;
        let mut done = 0;
        while done < buf.len() {
            let block = u32::try_from(pos / REKEY)
                .map_err(|_| CryptoError::limit("Workbook stream bytes", u64::from(u32::MAX)))?;
            let off = pos % REKEY;
            if self.block != Some(block) || self.at > off || self.cipher.is_none() {
                self.cipher = Some(self.base.cipher(block)?);
                self.block = Some(block);
                self.at = 0;
            }
            let Some(cipher) = self.cipher.as_mut() else {
                break;
            };
            if off > self.at {
                cipher.write_keystream(&mut self.scratch[..off - self.at]);
            }
            let n = (buf.len() - done).min(REKEY - off);
            cipher.apply_keystream(&mut buf[done..done + n]);
            self.at = off + n;
            done += n;
            pos += n;
        }
        Ok(())
    }
}
