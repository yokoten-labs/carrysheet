//! Errors with three parts: what happened, why, and how to fix it.

use std::fmt;

/// Everything that can stop a file from being detected, decrypted or
/// encrypted.
///
/// Each error explains itself in three parts: [`CryptoError::what`],
/// [`CryptoError::why`] and [`CryptoError::fix`]. `Display` joins all three.
/// Errors never contain a password, a key or decrypted bytes, so they are
/// safe to log. A wrong password says only that: nothing about how close
/// it was or which check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoError {
    /// The bytes are not an OLE2 compound file. Encrypted `.xlsx` files
    /// and all `.xls` files are compound files.
    NotCompoundFile,
    /// The compound file is damaged, or its reader gave up on it.
    Container {
        /// What the compound file reader reported.
        detail: String,
    },
    /// The file is not encrypted, so there is nothing to decrypt.
    NotEncrypted,
    /// The file is protected by Microsoft Rights Management (a Purview
    /// sensitivity label with encryption, or Information Rights
    /// Management). No password can open it.
    RightsManagement,
    /// The file is encrypted in a way this crate does not handle.
    Unsupported {
        /// The method, in words, such as `extensible encryption`.
        method: String,
    },
    /// The file needs a password and none was given (Excel's built-in
    /// default password did not open it either).
    PasswordRequired,
    /// The password does not open the file.
    WrongPassword,
    /// The password is longer than Office allows.
    PasswordTooLong {
        /// The most characters (UTF-16 code units) allowed.
        max: usize,
    },
    /// A part of the encryption data does not follow the format.
    Malformed {
        /// Which part, such as `EncryptionInfo` or `FILEPASS record`.
        part: &'static str,
        /// What is wrong with it.
        detail: String,
    },
    /// A safety limit from [`Limits`](crate::Limits) was reached.
    LimitExceeded {
        /// Which limit, such as `spin count`.
        limit: &'static str,
        /// The limit's value.
        max: u64,
    },
    /// The password was right, but the encrypted package does not match
    /// its integrity check (HMAC): it was changed or damaged after it was
    /// encrypted.
    IntegrityFailed,
    /// The agile file has no integrity check (`dataIntegrity`), so a
    /// change to it could not be seen; it is refused.
    IntegrityMissing,
    /// Saving would drop the certificate key encryptor of an `.xlsx`
    /// that could also be opened with a certificate.
    CertificateWouldBeLost,
    /// The caller asked for something this crate cannot do as asked.
    InvalidArgument {
        /// What is wrong with the request.
        detail: String,
    },
    /// The operating system (or browser) gave no random bytes for a new
    /// salt or key.
    Random {
        /// What the random source reported.
        detail: String,
    },
    /// Writing the encrypted compound file failed.
    WriteFailed {
        /// What the compound file writer reported.
        detail: String,
    },
}

impl CryptoError {
    /// What happened, in one sentence.
    pub fn what(&self) -> String {
        match self {
            CryptoError::NotCompoundFile => {
                "The file is not an OLE2 compound file, so it is not an encrypted Office file.".into()
            }
            CryptoError::Container { .. } => {
                "The compound file that holds the encrypted workbook is damaged.".into()
            }
            CryptoError::NotEncrypted => "The file is not encrypted.".into(),
            CryptoError::RightsManagement => {
                "This workbook is protected by Microsoft Rights Management (a sensitivity label or Information Rights Management), so Carrysheet cannot open it.".into()
            }
            CryptoError::Unsupported { method } => {
                format!("The workbook is encrypted with {method}, which Carrysheet cannot decrypt.")
            }
            CryptoError::PasswordRequired => "The workbook is protected with a password.".into(),
            CryptoError::WrongPassword => "The password is wrong.".into(),
            CryptoError::PasswordTooLong { max } => {
                format!("The password is longer than {max} characters.")
            }
            CryptoError::Malformed { part, .. } => {
                format!("The encryption data in the file is damaged ({part}).")
            }
            CryptoError::LimitExceeded { limit, max } => {
                format!("The encrypted file is over the limit on {limit} ({max}).")
            }
            CryptoError::IntegrityFailed => {
                "The encrypted workbook was changed or damaged after it was saved.".into()
            }
            CryptoError::IntegrityMissing => {
                "The encrypted workbook has no integrity check, so changes to it cannot be detected.".into()
            }
            CryptoError::CertificateWouldBeLost => {
                "Saving would remove the certificate that can also open this workbook.".into()
            }
            CryptoError::InvalidArgument { detail } => {
                format!("Carrysheet was asked to do something it cannot: {detail}.")
            }
            CryptoError::Random { .. } => {
                "No random numbers were available to encrypt the file.".into()
            }
            CryptoError::WriteFailed { .. } => "The encrypted file could not be written.".into(),
        }
    }

    /// Why it happened.
    pub fn why(&self) -> String {
        match self {
            CryptoError::NotCompoundFile => {
                "Password-protected .xlsx files and all .xls files start with the compound file signature D0 CF 11 E0; these bytes do not.".into()
            }
            CryptoError::Container { detail } => {
                format!("The compound file reader said: {detail}.")
            }
            CryptoError::NotEncrypted => {
                "It has no EncryptionInfo stream and no FILEPASS record.".into()
            }
            CryptoError::RightsManagement => {
                "Its contents are locked with a key that only Microsoft's rights service hands out, after it checks which account you are signed in with. A password cannot unlock it, and Carrysheet does not sign in to that service.".into()
            }
            CryptoError::Unsupported { method } => format!(
                "Carrysheet decrypts agile and standard encryption (.xlsx) and RC4, RC4 CryptoAPI and XOR obfuscation (.xls); {method} is none of these."
            ),
            CryptoError::PasswordRequired => {
                "No password was given, and Excel's built-in default password did not open it.".into()
            }
            CryptoError::WrongPassword => {
                "The file's password check did not match the password given.".into()
            }
            CryptoError::PasswordTooLong { .. } => {
                "Excel accepts at most 255 characters for a password to open (15 for old .xls XOR obfuscation).".into()
            }
            CryptoError::Malformed { detail, .. } => format!("{detail}."),
            CryptoError::LimitExceeded { .. } => {
                "Limits stop damaged or hostile files from using all memory or time; real Excel files stay far below them.".into()
            }
            CryptoError::IntegrityFailed => {
                "Agile encryption stores a keyed checksum (HMAC) of the encrypted package; it does not match the bytes in the file.".into()
            }
            CryptoError::IntegrityMissing => {
                "The program that wrote the file left the check out, or it was removed from the file.".into()
            }
            CryptoError::CertificateWouldBeLost => {
                "The file opens with a password or with a certificate; Carrysheet saves only the password key, so people who open it with the certificate would be locked out.".into()
            }
            CryptoError::InvalidArgument { .. } => {
                "The request does not fit the file or the format.".into()
            }
            CryptoError::Random { detail } => {
                format!("Each save needs a fresh random salt and key; the random source said: {detail}.")
            }
            CryptoError::WriteFailed { detail } => {
                format!("The compound file writer said: {detail}.")
            }
        }
    }

    /// How to fix it.
    pub fn fix(&self) -> String {
        match self {
            CryptoError::NotCompoundFile | CryptoError::NotEncrypted => {
                "Open the file without a password.".into()
            }
            CryptoError::RightsManagement => {
                "Open it in Excel while signed in with an account that has access, or ask the file's owner to remove the label (or pick one without encryption) and send the file again.".into()
            }
            CryptoError::Unsupported { .. } => {
                "Open the file in Excel, then save it again with File > Info > Protect Workbook > Encrypt with Password, which uses agile encryption.".into()
            }
            CryptoError::PasswordRequired => "Give the password to open the file.".into(),
            CryptoError::WrongPassword => {
                "Check the password (it is case-sensitive) and try again.".into()
            }
            CryptoError::PasswordTooLong { .. } => "Use the password the file was saved with.".into(),
            CryptoError::LimitExceeded { .. } => {
                "If the file is trusted, raise the limit in `Limits`; otherwise treat the file as hostile.".into()
            }
            CryptoError::IntegrityFailed => {
                "Do not trust this copy; get the file again from where it came from, or restore it from a backup.".into()
            }
            CryptoError::IntegrityMissing => {
                "Save the file again in Excel with a password, or get a fresh copy from where it came from.".into()
            }
            CryptoError::CertificateWouldBeLost => {
                "Save it in Excel, or confirm that the certificate may be dropped (`SaveKey::allow_dropping_certificate`).".into()
            }
            CryptoError::InvalidArgument { .. } => {
                "Change the request as the message says.".into()
            }
            CryptoError::Random { .. } => {
                "Try again; if it keeps failing, the system's random number source is broken.".into()
            }
            CryptoError::WriteFailed { .. } => {
                "Check that there is free memory, then save again.".into()
            }
            CryptoError::Container { .. } | CryptoError::Malformed { .. } => {
                "Get the file again from where it came from, or restore it from a backup.".into()
            }
        }
    }

    pub(crate) fn malformed(part: &'static str, detail: impl Into<String>) -> Self {
        CryptoError::Malformed {
            part,
            detail: detail.into(),
        }
    }

    pub(crate) fn limit(limit: &'static str, max: u64) -> Self {
        CryptoError::LimitExceeded { limit, max }
    }

    pub(crate) fn invalid(detail: impl Into<String>) -> Self {
        CryptoError::InvalidArgument {
            detail: detail.into(),
        }
    }

    pub(crate) fn unsupported(method: impl Into<String>) -> Self {
        CryptoError::Unsupported {
            method: method.into(),
        }
    }
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.what(), self.why(), self.fix())
    }
}

impl std::error::Error for CryptoError {}
