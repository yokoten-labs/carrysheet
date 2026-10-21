//! Errors in three parts: what happened, why, and how to fix it.

use std::fmt;

use crate::name::show;

/// Why reading or writing a compound file failed.
///
/// [`CfbError::what`], [`CfbError::why`] and [`CfbError::fix`] give the
/// three parts of a message for people and agents; `Display` gives the
/// first two.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CfbError {
    /// The bytes are not a compound file at all.
    NotCompoundFile {
        /// What the bytes are instead.
        detail: String,
    },
    /// A header value this reader does not support, such as version 2.
    Unsupported {
        /// The value and what was expected.
        detail: String,
    },
    /// The structure is damaged.
    Damaged {
        /// Where: `header`, `DIFAT`, `FAT`, `mini FAT`, `directory`,
        /// `mini stream`, or a stream's path.
        part: String,
        /// What is wrong with it.
        detail: String,
    },
    /// A [`Limits`](crate::Limits) bound was reached.
    LimitExceeded {
        /// What was counted.
        what: &'static str,
        /// The bound.
        limit: u64,
    },
    /// No storage or stream at this path.
    NotFound {
        /// The path asked for.
        path: String,
    },
    /// A storage where a stream was needed, or the other way round.
    WrongKind {
        /// The path asked for.
        path: String,
        /// What was needed: `stream` or `storage`.
        expected: &'static str,
    },
    /// Writing: an entry with this name is already there.
    AlreadyExists {
        /// The path asked for.
        path: String,
    },
    /// Writing: a name the format cannot store.
    InvalidName {
        /// The name.
        name: String,
        /// Why it cannot be stored.
        why: &'static str,
    },
    /// Writing: more than the format can hold.
    TooLarge {
        /// What does not fit.
        detail: String,
    },
}

impl CfbError {
    pub(crate) fn damaged(part: impl Into<String>, detail: impl Into<String>) -> CfbError {
        CfbError::Damaged {
            part: part.into(),
            detail: detail.into(),
        }
    }

    pub(crate) fn limit(what: &'static str, limit: u64) -> CfbError {
        CfbError::LimitExceeded { what, limit }
    }

    /// What happened, in one sentence.
    pub fn what(&self) -> String {
        match self {
            CfbError::NotCompoundFile { .. } => {
                "The bytes are not a compound file (the OLE2 container of .xls files)".into()
            }
            CfbError::Unsupported { .. } => {
                "The compound file uses a variant this reader does not support".into()
            }
            CfbError::Damaged { part, .. } => {
                format!("The compound file is damaged in its {}", show(part))
            }
            CfbError::LimitExceeded { what, .. } => {
                format!("The compound file needs more {what} than the reader allows")
            }
            CfbError::NotFound { path } => format!("The compound file has no {}", show(path)),
            CfbError::WrongKind { path, expected } => {
                format!("{} in the compound file is not a {expected}", show(path))
            }
            CfbError::AlreadyExists { path } => format!("{} already exists", show(path)),
            CfbError::InvalidName { name, .. } => {
                format!(
                    "The name {} cannot be stored in a compound file",
                    show(name)
                )
            }
            CfbError::TooLarge { .. } => "The compound file would be too large".into(),
        }
    }

    /// Why it happened.
    pub fn why(&self) -> String {
        match self {
            CfbError::NotCompoundFile { detail }
            | CfbError::Unsupported { detail }
            | CfbError::Damaged { detail, .. }
            | CfbError::TooLarge { detail } => detail.clone(),
            CfbError::LimitExceeded { what, limit } => format!(
                "it needs more than {limit} {what}; the limit stops files built to exhaust memory or time"
            ),
            CfbError::NotFound { .. } => {
                "no storage or stream has that path (names compare without case)".into()
            }
            CfbError::WrongKind { expected, .. } => {
                if *expected == "stream" {
                    "it is a storage, which holds entries rather than bytes".into()
                } else {
                    "it is a stream, which holds bytes rather than entries".into()
                }
            }
            CfbError::AlreadyExists { .. } => {
                "a storage holds each name once, compared without case".into()
            }
            CfbError::InvalidName { why, .. } => (*why).into(),
        }
    }

    /// How to fix it.
    pub fn fix(&self) -> String {
        match self {
            CfbError::NotCompoundFile { .. } => {
                "Pass the whole file (.xls, vbaProject.bin or an encrypted Office file), and check that it was not cut short or converted on the way.".into()
            }
            CfbError::Unsupported { .. } | CfbError::Damaged { .. } => {
                "Get a fresh copy of the file from where it came from, or restore it from a backup.".into()
            }
            CfbError::LimitExceeded { .. } => {
                "If the file is genuine, raise the bound in carrysheet_cfb::Limits; otherwise treat the file as hostile.".into()
            }
            CfbError::NotFound { .. } | CfbError::WrongKind { .. } => {
                "Check the path; CompoundFile::walk(\"/\") lists every entry.".into()
            }
            CfbError::AlreadyExists { .. } => {
                "Use Builder::put_stream to replace a stream, or choose another name.".into()
            }
            CfbError::InvalidName { .. } => {
                "Use 1 to 31 UTF-16 code units, without '/', '\\', ':' or '!'.".into()
            }
            CfbError::TooLarge { .. } => {
                "Store less in one file; streams over 4 GB need Version::V4.".into()
            }
        }
    }
}

impl fmt::Display for CfbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.what(), self.why())
    }
}

impl std::error::Error for CfbError {}
