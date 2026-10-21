//! Errors returned by the reader.
//!
//! Every error says what went wrong, why, and how to fix it, and carries the
//! byte offset in the workbook stream when there is one.

use std::fmt;

/// How a file is protected, as declared by its FILEPASS record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// XOR obfuscation (BIFF2 to BIFF8).
    Xor,
    /// RC4 with the BIFF8 standard method.
    Rc4,
    /// RC4 with a CryptoAPI provider (Excel 2002 and later).
    Rc4CryptoApi,
    /// A FILEPASS record we could not classify.
    Unknown,
}

impl Encryption {
    fn describe(self) -> &'static str {
        match self {
            Encryption::Xor => "XOR obfuscation",
            Encryption::Rc4 => "RC4 encryption",
            Encryption::Rc4CryptoApi => "RC4 CryptoAPI encryption",
            Encryption::Unknown => "an unknown encryption method",
        }
    }
}

/// What kind of failure stopped the read.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BiffErrorKind {
    /// The bytes are neither an OLE2 compound file nor a raw BIFF stream.
    NotXls { detail: String },
    /// The OLE2 compound file structure is broken.
    Container { detail: String },
    /// The compound file has no `Workbook` or `Book` stream.
    NoWorkbookStream { streams: Vec<String> },
    /// The workbook is password protected (FILEPASS record).
    Encrypted { method: Encryption },
    /// A BOF record names a BIFF version this reader does not know.
    UnsupportedVersion { bof_type: u16, version: u16 },
    /// A record ends before a field it must contain.
    Truncated {
        record: &'static str,
        record_type: u16,
        needed: usize,
        available: usize,
    },
    /// A record holds a value that cannot be right.
    Malformed {
        record: &'static str,
        record_type: u16,
        detail: String,
    },
    /// The stream ended in the middle of a record header or substream.
    UnexpectedEnd { detail: String },
    /// A safety limit was reached.
    LimitExceeded { limit: &'static str, max: u64 },
}

/// A read failure, with the stream offset where it happened when known.
#[derive(Debug, Clone, PartialEq)]
pub struct BiffError {
    pub kind: BiffErrorKind,
    /// Byte offset in the workbook stream (for raw BIFF2 to BIFF4 files, the file).
    pub offset: Option<u64>,
}

impl BiffError {
    pub(crate) fn new(kind: BiffErrorKind, offset: Option<u64>) -> Self {
        BiffError { kind, offset }
    }

    pub(crate) fn at(kind: BiffErrorKind, offset: u64) -> Self {
        BiffError {
            kind,
            offset: Some(offset),
        }
    }

    pub(crate) fn limit(limit: &'static str, max: usize, offset: Option<u64>) -> Self {
        BiffError {
            kind: BiffErrorKind::LimitExceeded {
                limit,
                max: max as u64,
            },
            offset,
        }
    }

    /// What happened, in one sentence.
    pub fn what(&self) -> String {
        match &self.kind {
            BiffErrorKind::NotXls { .. } => "The file is not a legacy Excel (.xls) workbook".into(),
            BiffErrorKind::Container { .. } => {
                "The OLE2 container of the .xls file is damaged".into()
            }
            BiffErrorKind::NoWorkbookStream { .. } => {
                "The OLE2 file has no Excel workbook stream".into()
            }
            BiffErrorKind::Encrypted { .. } => {
                "The workbook is encrypted with a password; decryption is not supported yet".into()
            }
            BiffErrorKind::UnsupportedVersion { .. } => {
                "The workbook uses a BIFF version this reader does not know".into()
            }
            BiffErrorKind::Truncated { record, .. } => format!("A {record} record is too short"),
            BiffErrorKind::Malformed { record, .. } => format!("A {record} record is invalid"),
            BiffErrorKind::UnexpectedEnd { .. } => "The workbook stream ends too early".into(),
            BiffErrorKind::LimitExceeded { limit, .. } => {
                format!("The workbook exceeds the reader's limit on {limit}")
            }
        }
    }

    /// Why it happened.
    pub fn why(&self) -> String {
        match &self.kind {
            BiffErrorKind::NotXls { detail } => detail.clone(),
            BiffErrorKind::Container { detail } => detail.clone(),
            BiffErrorKind::NoWorkbookStream { streams } => {
                if streams.is_empty() {
                    "the compound file contains no streams".into()
                } else {
                    format!(
                        "it contains only these streams: {}; it may be another Office format (Word, PowerPoint) or an Excel 2007+ encrypted package",
                        streams.join(", ")
                    )
                }
            }
            BiffErrorKind::Encrypted { method } => {
                format!("the FILEPASS record declares {}", method.describe())
            }
            BiffErrorKind::UnsupportedVersion { bof_type, version } => {
                format!("the BOF record has type 0x{bof_type:04X} and version 0x{version:04X}")
            }
            BiffErrorKind::Truncated {
                record_type,
                needed,
                available,
                ..
            } => format!(
                "record 0x{record_type:04X} needs {needed} bytes but only {available} are present"
            ),
            BiffErrorKind::Malformed {
                record_type,
                detail,
                ..
            } => format!("record 0x{record_type:04X}: {detail}"),
            BiffErrorKind::UnexpectedEnd { detail } => detail.clone(),
            BiffErrorKind::LimitExceeded { max, .. } => {
                format!("the limit is {max}, set to protect against damaged or hostile files")
            }
        }
    }

    /// How to fix it.
    pub fn fix(&self) -> String {
        match &self.kind {
            BiffErrorKind::NotXls { .. } => {
                "Check that the file is an Excel 2.x to 2003 workbook; open .xlsx, .ods or .csv files with their own readers".into()
            }
            BiffErrorKind::Container { .. }
            | BiffErrorKind::Truncated { .. }
            | BiffErrorKind::Malformed { .. }
            | BiffErrorKind::UnexpectedEnd { .. } => {
                "The file is probably damaged or was cut short; get a fresh copy, or open it in Excel and save it again".into()
            }
            BiffErrorKind::NoWorkbookStream { .. } => {
                "Check that the file is an Excel workbook and not another Office document".into()
            }
            BiffErrorKind::Encrypted { .. } => {
                "Open the file in Excel with its password, remove the password (File > Info > Protect Workbook), and save it again".into()
            }
            BiffErrorKind::UnsupportedVersion { .. } => {
                "Open the file in Excel and save it as Excel 97-2003 (.xls) or .xlsx".into()
            }
            BiffErrorKind::LimitExceeded { .. } => {
                "If the file is genuine, read it with higher limits (read_with_limits)".into()
            }
        }
    }
}

impl fmt::Display for BiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.what())?;
        if let Some(offset) = self.offset {
            write!(f, " (at stream offset {offset})")?;
        }
        write!(f, ": {}. {}.", self.why(), self.fix())
    }
}

impl std::error::Error for BiffError {}
