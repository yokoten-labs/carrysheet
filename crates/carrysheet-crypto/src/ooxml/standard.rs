//! Standard encryption (ECMA-376, MS-OFFCRYPTO 2.3.4.5 - 2.3.4.9) and the
//! binary EncryptionHeader / EncryptionVerifier it shares with RC4
//! CryptoAPI in `.xls`.

use zeroize::Zeroizing;

use crate::detect::Protection;
use crate::error::CryptoError;
use crate::util::{AES_BLOCK, Aes, HashAlgorithm, Reader, ct_eq};
use crate::{Password, SaveKey, try_passwords};

pub(crate) const ALG_RC4: u32 = 0x6801;
pub(crate) const ALG_AES128: u32 = 0x660E;
pub(crate) const ALG_AES192: u32 = 0x660F;
pub(crate) const ALG_AES256: u32 = 0x6610;
pub(crate) const ALG_SHA1: u32 = 0x8004;

pub(crate) const F_CRYPTOAPI: u32 = 0x04;
pub(crate) const F_DOCPROPS: u32 = 0x08;
pub(crate) const F_EXTERNAL: u32 = 0x10;
pub(crate) const F_AES: u32 = 0x20;

/// Rounds of SHA-1 in standard key derivation (fixed by the spec).
const STANDARD_SPIN: u32 = 50_000;

/// EncryptionHeader (2.3.2).
#[derive(Debug, Clone)]
pub(crate) struct CryptoApiHeader {
    pub(crate) flags: u32,
    pub(crate) alg_id: u32,
    pub(crate) alg_id_hash: u32,
    pub(crate) key_bits: u32,
}

/// EncryptionVerifier (2.3.3).
#[derive(Debug, Clone)]
pub(crate) struct Verifier {
    pub(crate) salt: Vec<u8>,
    pub(crate) encrypted_verifier: Vec<u8>,
    pub(crate) verifier_hash_size: u32,
    pub(crate) encrypted_verifier_hash: Vec<u8>,
}

impl CryptoApiHeader {
    /// Reads `Flags`, `HeaderSize` and the header that follows them.
    pub(crate) fn parse(r: &mut Reader<'_>) -> Result<CryptoApiHeader, CryptoError> {
        let _outer_flags = r.u32("the flags")?;
        let size = r.u32("the header size")? as usize;
        if !(32..=4096).contains(&size) {
            return Err(CryptoError::malformed(
                "EncryptionHeader",
                format!("its size is {size} bytes; it must be 32 to 4096"),
            ));
        }
        let mut h = Reader::new(r.take(size, "the encryption header")?, "EncryptionHeader");
        let flags = h.u32("the flags")?;
        let _size_extra = h.u32("SizeExtra")?;
        let alg_id = h.u32("AlgID")?;
        let alg_id_hash = h.u32("AlgIDHash")?;
        let key_bits = h.u32("KeySize")?;
        // ProviderType, two reserved fields and the CSP name follow; they
        // do not change how the key is made.
        Ok(CryptoApiHeader {
            flags,
            alg_id,
            alg_id_hash,
            key_bits,
        })
    }
}

impl Verifier {
    /// Reads the verifier. `hash_len` is how many bytes the encrypted
    /// verifier hash takes: 32 for AES (20 padded to a block), 20 for RC4.
    pub(crate) fn parse(r: &mut Reader<'_>, hash_len: usize) -> Result<Verifier, CryptoError> {
        let salt_size = r.u32("the salt size")?;
        if salt_size != 16 {
            return Err(CryptoError::malformed(
                "EncryptionVerifier",
                format!("the salt is {salt_size} bytes; it must be 16"),
            ));
        }
        let salt = r.take(16, "the salt")?.to_vec();
        let encrypted_verifier = r.take(16, "the encrypted verifier")?.to_vec();
        let verifier_hash_size = r.u32("the verifier hash size")?;
        let encrypted_verifier_hash = r.take(hash_len, "the encrypted verifier hash")?.to_vec();
        Ok(Verifier {
            salt,
            encrypted_verifier,
            verifier_hash_size,
            encrypted_verifier_hash,
        })
    }
}

/// The parsed EncryptionInfo of standard encryption.
pub(crate) struct StandardInfo {
    header: CryptoApiHeader,
    verifier: Verifier,
}

pub(crate) fn parse(r: &mut Reader<'_>) -> Result<StandardInfo, CryptoError> {
    let header = CryptoApiHeader::parse(r)?;
    let verifier = Verifier::parse(r, 32)?;
    Ok(StandardInfo { header, verifier })
}

impl StandardInfo {
    pub(crate) fn protection(&self) -> Protection {
        Protection::Standard {
            key_bits: self.header.key_bits,
        }
    }

    fn check(&self) -> Result<usize, CryptoError> {
        let h = &self.header;
        if h.flags & F_EXTERNAL != 0 {
            return Err(CryptoError::unsupported(
                "external (third-party) encryption",
            ));
        }
        let aes = h.flags & F_AES != 0 || matches!(h.alg_id, ALG_AES128 | ALG_AES192 | ALG_AES256);
        if !aes || h.alg_id == ALG_RC4 {
            return Err(CryptoError::unsupported("standard encryption with RC4"));
        }
        if !matches!(h.alg_id_hash, 0 | ALG_SHA1) {
            return Err(CryptoError::unsupported(format!(
                "standard encryption with hash algorithm 0x{:04X}",
                h.alg_id_hash
            )));
        }
        let bits = match (h.alg_id, h.key_bits) {
            (0 | ALG_AES128, 128) | (ALG_AES192, 192) | (ALG_AES256, 256) => h.key_bits,
            (0, 192 | 256) => h.key_bits,
            (_, bits) => {
                return Err(CryptoError::malformed(
                    "EncryptionHeader",
                    format!(
                        "the AES key size {bits} does not match algorithm 0x{:04X}",
                        h.alg_id
                    ),
                ));
            }
        };
        if self.verifier.verifier_hash_size != 20 {
            return Err(CryptoError::malformed(
                "EncryptionVerifier",
                "the verifier hash size is not 20 (SHA-1)",
            ));
        }
        Ok(bits as usize / 8)
    }

    /// Makes the key and checks it against the verifier (2.3.4.7, 2.3.4.9).
    fn key(&self, password: &Password, key_len: usize) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let pw = password.utf16le()?;
        let key = standard_key(&self.verifier.salt, &pw, key_len);
        let aes = Aes::new(&key)?;
        let mut verifier = Zeroizing::new(self.verifier.encrypted_verifier.clone());
        aes.ecb_decrypt(&mut verifier);
        let mut hash = Zeroizing::new(self.verifier.encrypted_verifier_hash.clone());
        aes.ecb_decrypt(&mut hash);
        let expected = HashAlgorithm::Sha1.digest(&[&verifier]);
        if !ct_eq(&expected, &hash[..20]) {
            return Err(CryptoError::WrongPassword);
        }
        Ok(key)
    }

    pub(crate) fn decrypt(
        &self,
        package: &[u8],
        password: Option<&Password>,
    ) -> Result<(Vec<u8>, SaveKey), CryptoError> {
        let key_len = self.check()?;
        let (key, save) = try_passwords(password, true, |p| self.key(p, key_len))?;
        let mut r = Reader::new(package, "EncryptedPackage");
        let size = r.u64("the package size")?;
        let data = r.rest();
        let blocks = size
            .div_ceil(AES_BLOCK as u64)
            .saturating_mul(AES_BLOCK as u64);
        if blocks > data.len() as u64 {
            return Err(CryptoError::malformed(
                "EncryptedPackage",
                format!(
                    "it says the package is {size} bytes, but the stream holds only {} encrypted bytes",
                    data.len()
                ),
            ));
        }
        // `blocks <= data.len()`, so the output is bounded by the real data.
        let mut out = data[..blocks as usize].to_vec();
        Aes::new(&key)?.ecb_decrypt(&mut out);
        out.truncate(size as usize);
        Ok((out, save))
    }
}

/// Standard key derivation (2.3.4.7): SHA-1 spun 50,000 times, then the
/// CryptDeriveKey construction with 0x36 and 0x5C buffers.
fn standard_key(salt: &[u8], password: &[u8], key_len: usize) -> Zeroizing<Vec<u8>> {
    let h = HashAlgorithm::Sha1.spin(salt, password, STANDARD_SPIN);
    let hfinal = HashAlgorithm::Sha1.digest(&[&h, &0u32.to_le_bytes()]);
    let mut buf1 = Zeroizing::new([0x36u8; 64]);
    let mut buf2 = Zeroizing::new([0x5Cu8; 64]);
    for (i, b) in hfinal.iter().enumerate() {
        buf1[i] ^= b;
        buf2[i] ^= b;
    }
    let x1 = HashAlgorithm::Sha1.digest(&[&buf1[..]]);
    let x2 = HashAlgorithm::Sha1.digest(&[&buf2[..]]);
    let mut x3 = Zeroizing::new(Vec::with_capacity(40));
    x3.extend_from_slice(&x1);
    x3.extend_from_slice(&x2);
    x3.truncate(key_len);
    x3
}
