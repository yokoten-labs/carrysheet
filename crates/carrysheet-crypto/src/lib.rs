//! Detect, decrypt and re-encrypt password-protected Excel files.
//!
//! Follows Microsoft's public specifications MS-OFFCRYPTO and MS-XLS:
//!
//! | File | Method | Detect | Decrypt | Encrypt |
//! |---|---|---|---|---|
//! | `.xlsx`, `.xlsm`, `.xlsb` | Agile (AES, SHA-1/256/384/512) | yes | yes, HMAC checked | AES-256 + SHA-512 |
//! | | Standard (ECMA-376, AES + SHA-1) | yes | yes | (saved as agile) |
//! | | Extensible, certificate-only | yes | no | |
//! | | Rights Management / sensitivity label | yes, explained | no | |
//! | `.xls` (BIFF8) | RC4 CryptoAPI | yes | yes, with encrypted properties | RC4 CryptoAPI, 128-bit |
//! | | RC4 (MD5) | yes | yes | (saved as CryptoAPI) |
//! | | XOR obfuscation (method 1) | yes | yes | (saved as CryptoAPI) |
//!
//! An encrypted `.xlsx` is not a zip: it is an OLE2 compound file with an
//! `EncryptionInfo` stream and an `EncryptedPackage` stream, the zip
//! inside. An encrypted `.xls` is a normal `.xls` whose `Workbook` stream
//! has a FILEPASS record and encrypted record bodies.
//!
//! ```no_run
//! use carrysheet_crypto::{decrypt_ooxml, detect, Limits, Password, Protection};
//!
//! # fn demo(file: Vec<u8>) -> Result<(), carrysheet_crypto::CryptoError> {
//! let limits = Limits::default();
//! match detect(&file, &limits)? {
//!     None => { /* not encrypted: read it as usual */ }
//!     Some(Protection::RightsManagement) => {
//!         // Explain in plain words; no password can open it.
//!         let e = carrysheet_crypto::CryptoError::RightsManagement;
//!         eprintln!("{}\n{}\n{}", e.what(), e.why(), e.fix());
//!     }
//!     Some(_) => {
//!         let opened = decrypt_ooxml(&file, Some(&Password::new("secret")), &limits)?;
//!         let zip: &[u8] = &opened.package;
//!         # let _ = zip;
//!         // ... edit, then save with the same protection:
//!         let saved = opened.key.encrypt_ooxml(&opened.package)?;
//!         # let _ = saved;
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Keeping protection on save
//!
//! Every successful decryption returns a [`SaveKey`]: the password that
//! opened the file and what the file had. A save through it is never
//! weaker than the file that was opened:
//!
//! - `.xlsx`: [`SaveKey::encrypt_ooxml`] writes agile encryption, AES-256
//!   and SHA-512, with fresh random salts and key, and at least
//!   [`MIN_SAVE_SPIN_COUNT`] (100,000, what Excel writes) key-stretching
//!   rounds, or the opened file's own count when that was higher. A file
//!   opened with more than 10,000,000 rounds (under a raised
//!   [`Limits::max_spin_count`]) is saved at 10,000,000, the most
//!   [`encrypt_ooxml`] accepts; a file opened under a limit below 100,000
//!   is saved at that limit. Standard encryption is
//!   upgraded to agile. A file that could also be opened with a
//!   certificate is refused ([`CryptoError::CertificateWouldBeLost`])
//!   unless the caller allows it with
//!   [`SaveKey::allow_dropping_certificate`].
//! - `.xls`: [`SaveKey::biff_encryptor`] writes RC4 CryptoAPI 128-bit
//!   (XOR and RC4 files are upgraded), and keeps the document properties
//!   encrypted when they were encrypted.
//!
//! Files that opened with Excel's built-in default password
//! ([`DEFAULT_PASSWORD`], "VelvetSweatshop", which Excel uses for files
//! that are only write-protected) are saved with it again.
//!
//! # Hostile input
//!
//! Every size is checked against the data that is really there and
//! against [`Limits`]; nothing is allocated from a declared size alone.
//! Decrypted output is never longer than the encrypted bytes in the file,
//! the spin count (key-stretching rounds) is capped at
//! [`Limits::max_spin_count`], the `EncryptionInfo` XML is read with a
//! size and event budget, and the compound file reader (carrysheet-cfb)
//! returns errors instead of panicking. Errors never contain the password
//! or key material, and a wrong password is reported only as wrong.

mod biff;
mod detect;
mod error;
mod ooxml;
mod util;

pub use biff::{
    BiffEncryptor, DecryptedBiff, ENCRYPTION_STREAM, ProtectedStream, decrypt_biff,
    decrypt_biff_streams, detect_filepass, detect_workbook_stream, empty_document_summary,
    encrypt_biff,
};
pub use detect::{AgileSummary, BiffEncryption, Protection, detect};
pub use error::CryptoError;
pub use ooxml::{DecryptedPackage, EncryptOptions, decrypt_ooxml, encrypt_ooxml};
pub use util::HashAlgorithm;

use std::fmt;

use zeroize::Zeroizing;

/// Excel's built-in password, used for files that are only
/// write-protected ("Read-only recommended" or a password to modify).
/// Excel opens such files without asking, and so does this crate: it is
/// tried automatically whenever a file is decrypted.
pub const DEFAULT_PASSWORD: &str = "VelvetSweatshop";

/// The longest password Office accepts, in UTF-16 code units.
pub const MAX_PASSWORD_LEN: usize = 255;

/// Safety limits. The defaults are far above real Excel files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of the whole input (the compound file, or the `Workbook`
    /// stream for `.xls`).
    pub max_input_bytes: usize,
    /// Bytes of the `EncryptionInfo` stream (Excel writes about 1 KB; a
    /// certificate key encryptor adds a few KB).
    pub max_info_bytes: usize,
    /// XML events (elements, text, comments) in agile `EncryptionInfo`.
    pub max_xml_events: usize,
    /// Key-stretching rounds a file may ask for. Excel uses 100,000. The
    /// default cap, 10,000,000, takes a few seconds per password tried
    /// and stops a file from making the reader hash for hours. A server
    /// that opens files for others (such as `carrysheet mcp serve`)
    /// should pass a lower cap, for example 1,000,000.
    ///
    /// Excel's default password is tried after a wrong given password
    /// only when the file asks for at most 100,000 rounds, so a wrong
    /// password never costs more than one stretch of a costly file.
    pub max_spin_count: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_input_bytes: 1 << 30,
            max_info_bytes: 1 << 20,
            max_xml_events: 10_000,
            max_spin_count: 10_000_000,
        }
    }
}

/// A password. Its memory is wiped when it is dropped, and `Debug` never
/// shows it. It has no `==`: compare passwords through the file's
/// verifier, which this crate does in constant time.
#[derive(Clone)]
pub struct Password(Zeroizing<String>);

impl Password {
    /// Wraps a password.
    pub fn new(password: impl Into<String>) -> Password {
        Password(Zeroizing::new(password.into()))
    }

    /// Excel's built-in default password ([`DEFAULT_PASSWORD`]).
    pub fn excel_default() -> Password {
        Password::new(DEFAULT_PASSWORD)
    }

    /// The password text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is Excel's built-in default password.
    pub fn is_excel_default(&self) -> bool {
        self.as_str() == DEFAULT_PASSWORD
    }

    /// UTF-16LE bytes, checked against [`MAX_PASSWORD_LEN`].
    pub(crate) fn utf16le(&self) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let units = self.0.encode_utf16().count();
        if units > MAX_PASSWORD_LEN {
            return Err(CryptoError::PasswordTooLong {
                max: MAX_PASSWORD_LEN,
            });
        }
        let mut out = Zeroizing::new(Vec::with_capacity(units * 2));
        for u in self.0.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        Ok(out)
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<hidden>)")
    }
}

/// The fewest key-stretching rounds a [`SaveKey`] writes (what Excel
/// writes).
pub const MIN_SAVE_SPIN_COUNT: u32 = 100_000;

/// What a save needs to keep a decrypted file protected: the password
/// that opened it and how strongly the file was protected. `Debug` never
/// shows the password.
#[derive(Clone, Debug)]
pub struct SaveKey {
    password: Password,
    spin_count: u32,
    certificate: bool,
    drop_certificate: bool,
    doc_props_encrypted: bool,
}

impl SaveKey {
    /// A key for saving with this password: agile with
    /// [`MIN_SAVE_SPIN_COUNT`] rounds, `.xls` with plain document
    /// properties.
    pub fn new(password: Password) -> SaveKey {
        SaveKey {
            password,
            spin_count: MIN_SAVE_SPIN_COUNT,
            certificate: false,
            drop_certificate: false,
            doc_props_encrypted: false,
        }
    }

    /// Whether the file opened with Excel's built-in default password
    /// (it was only write-protected, not secret).
    pub fn is_excel_default(&self) -> bool {
        self.password.is_excel_default()
    }

    /// The password that opened the file.
    pub fn password(&self) -> &Password {
        &self.password
    }

    /// The key-stretching rounds an `.xlsx` save uses: the opened file's
    /// count, at least [`MIN_SAVE_SPIN_COUNT`] (or the opening limit, when
    /// lower) and at most 10,000,000, the most [`encrypt_ooxml`] accepts.
    pub fn spin_count(&self) -> u32 {
        self.spin_count
    }

    /// Whether the opened `.xlsx` could also be opened with a certificate.
    /// A save writes only the password key, so certificate holders would
    /// lose access.
    pub fn had_certificate(&self) -> bool {
        self.certificate
    }

    /// Whether the opened `.xls` had its document properties encrypted.
    pub fn doc_props_encrypted(&self) -> bool {
        self.doc_props_encrypted
    }

    /// Allows [`SaveKey::encrypt_ooxml`] to save a file that could also be
    /// opened with a certificate, with the password key only.
    pub fn allow_dropping_certificate(mut self) -> SaveKey {
        self.drop_certificate = true;
        self
    }

    pub(crate) fn with_spin_count(mut self, original: u32, limit: u32) -> SaveKey {
        let floor = MIN_SAVE_SPIN_COUNT.min(limit);
        self.spin_count = original
            .max(floor)
            .min(Limits::default().max_spin_count)
            .max(1);
        self
    }

    pub(crate) fn with_certificate(mut self, certificate: bool) -> SaveKey {
        self.certificate = certificate;
        self
    }

    pub(crate) fn with_doc_props_encrypted(mut self, encrypted: bool) -> SaveKey {
        self.doc_props_encrypted = encrypted;
        self
    }

    /// Encrypts a package (the zip bytes of an `.xlsx`, `.xlsm` or
    /// `.xlsb`) with agile encryption, AES-256 and SHA-512, at
    /// [`SaveKey::spin_count`] rounds.
    pub fn encrypt_ooxml(&self, package: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.certificate && !self.drop_certificate {
            return Err(CryptoError::CertificateWouldBeLost);
        }
        let options = EncryptOptions {
            spin_count: self.spin_count,
        };
        encrypt_ooxml(package, &self.password, &options)
    }

    /// An encryptor for the `.xls` streams, with fresh random salt, that
    /// keeps the document properties encrypted when they were.
    pub fn biff_encryptor(&self) -> Result<BiffEncryptor, CryptoError> {
        BiffEncryptor::new(&self.password, self.doc_props_encrypted)
    }
}

/// Tries the given password, then Excel's default password, with `open`.
///
/// `open` returns [`CryptoError::WrongPassword`] when a password does not
/// fit; any other error stops the search. With `retry_default` false, the
/// default password is tried only when no password was given (so a
/// costly file costs one key stretch per call, not two).
pub(crate) fn try_passwords<T>(
    password: Option<&Password>,
    retry_default: bool,
    mut open: impl FnMut(&Password) -> Result<T, CryptoError>,
) -> Result<(T, SaveKey), CryptoError> {
    let default = Password::excel_default();
    let mut candidates: Vec<&Password> = Vec::with_capacity(2);
    if let Some(p) = password {
        candidates.push(p);
    }
    let given_default = password.is_some_and(Password::is_excel_default);
    if password.is_none() || (retry_default && !given_default) {
        candidates.push(&default);
    }
    for candidate in candidates {
        match open(candidate) {
            Ok(t) => return Ok((t, SaveKey::new(candidate.clone()))),
            Err(CryptoError::WrongPassword) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(if password.is_some() {
        CryptoError::WrongPassword
    } else {
        CryptoError::PasswordRequired
    })
}
