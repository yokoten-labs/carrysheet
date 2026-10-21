//! Errors with three parts: what happened, why, and how to fix it.

use std::fmt;

/// Everything that stops a Numbers document from being read.
///
/// Each error explains itself in three parts: [`NumbersError::what`],
/// [`NumbersError::why`] and [`NumbersError::fix`]. `Display` joins all
/// three. Errors name parts of the package (such as `Index/Document.iwa`)
/// and object numbers, never cell contents, sheet names or file paths, so
/// they are safe to log.
///
/// Problems that affect only one cell are not errors: the cell is left empty
/// and the problem is listed in [`Document::warnings`](crate::Document::warnings).
#[derive(Debug)]
#[non_exhaustive]
pub enum NumbersError {
    /// Opening or reading the file or folder failed.
    Io(std::io::Error),
    /// The input is not a Numbers document at all.
    NotNumbers {
        /// What was looked for and not found.
        reason: &'static str,
    },
    /// The document is protected with a password.
    PasswordProtected,
    /// The input goes over one of the [`Limits`](crate::Limits).
    TooLarge {
        /// What was counted, such as "bytes after decompression".
        what: &'static str,
        /// The limit that was reached.
        limit: u64,
    },
    /// A part of the package is damaged: bad zip data, a bad IWA chunk or a
    /// protobuf message that does not decode.
    Damaged {
        /// The part inside the package, such as `Index/Document.iwa`; empty
        /// when the document file itself (its zip) is damaged.
        part: String,
        /// What is wrong with it.
        detail: String,
    },
    /// An object that the document needs is not in the package.
    MissingObject {
        /// The object number.
        id: u64,
        /// What needed it, such as "sheet 1".
        needed_for: String,
    },
    /// An object has a different type than the reference to it expects.
    WrongObjectType {
        /// The object number.
        id: u64,
        /// The type that was expected, such as `TST.TableModelArchive`.
        expected: &'static str,
        /// The type number found in the package.
        found: u32,
    },
    /// The document uses a storage format this version does not read.
    Unsupported {
        /// What is not supported.
        feature: &'static str,
    },
}

impl NumbersError {
    /// What happened, in one sentence.
    pub fn what(&self) -> String {
        match self {
            Self::Io(e) => format!("Reading the document failed: {e}."),
            Self::NotNumbers { reason } => {
                format!("This is not a Numbers document: {reason}.")
            }
            Self::PasswordProtected => "The Numbers document is protected with a password.".into(),
            Self::TooLarge { what, limit } => {
                format!("The document goes over the limit of {limit} {what}.")
            }
            Self::Damaged { part, detail } if part.is_empty() => {
                format!("The document file is damaged: {detail}.")
            }
            Self::Damaged { part, detail } => {
                format!("The part {part} of the document is damaged: {detail}.")
            }
            Self::MissingObject { id, needed_for } => {
                format!("Object {id}, needed for {needed_for}, is missing from the document.")
            }
            Self::WrongObjectType {
                id,
                expected,
                found,
            } => format!("Object {id} should be a {expected} but has type {found}."),
            Self::Unsupported { feature } => {
                format!("The document uses {feature}, which this reader does not support yet.")
            }
        }
    }

    /// Why it is a problem.
    pub fn why(&self) -> String {
        match self {
            Self::Io(_) => "The operating system reported an error.".into(),
            Self::NotNumbers { .. } => {
                "A Numbers document is a zip file or a folder with an Index part.".into()
            }
            Self::PasswordProtected => {
                "Its contents are encrypted and this reader does not decrypt them.".into()
            }
            Self::TooLarge { .. } => {
                "Limits stop a damaged or hostile file from using up all memory.".into()
            }
            Self::Damaged { .. } | Self::MissingObject { .. } | Self::WrongObjectType { .. } => {
                "The file was probably cut short, changed by another program, or damaged.".into()
            }
            Self::Unsupported { .. } => {
                "Reading it anyway could show wrong values instead of the real ones.".into()
            }
        }
    }

    /// How to fix it.
    pub fn fix(&self) -> String {
        match self {
            Self::Io(_) => "Check the path and the permissions, then try again.".into(),
            Self::NotNumbers { .. } => {
                "Check that the file is a .numbers document saved by Apple Numbers.".into()
            }
            Self::PasswordProtected => {
                "Open it in Numbers, remove the password (File > Change Password), save, and try again."
                    .into()
            }
            Self::TooLarge { .. } => {
                "Split the document in Numbers, or raise the limit in `Limits`.".into()
            }
            Self::Damaged { .. } | Self::MissingObject { .. } | Self::WrongObjectType { .. } => {
                "Open the document in Numbers and save it again, or use an earlier copy.".into()
            }
            Self::Unsupported { .. } => {
                "In Numbers, export the document to Excel or CSV (File > Export To) and read that instead."
                    .into()
            }
        }
    }
}

impl fmt::Display for NumbersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.what(), self.why(), self.fix())
    }
}

impl std::error::Error for NumbersError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for NumbersError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Shorthand for a damaged part. Control characters in the part name
/// (which comes from the file) are replaced, so the message is safe to print.
pub(crate) fn damaged(part: &str, detail: impl Into<String>) -> NumbersError {
    NumbersError::Damaged {
        part: part
            .chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect(),
        detail: cut(detail.into()),
    }
}

/// The longest detail kept; library errors (prost, zip) can be long.
const MAX_DETAIL: usize = 200;

fn cut(mut detail: String) -> String {
    if let Some((at, _)) = detail.char_indices().nth(MAX_DETAIL) {
        detail.truncate(at);
        detail.push('…');
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damaged_messages() {
        let outer = damaged("", "the zip directory cannot be read");
        assert_eq!(
            outer.what(),
            "The document file is damaged: the zip directory cannot be read."
        );
        let part = damaged("Index/Tables/Tile.iwa", "a chunk is cut short");
        assert_eq!(
            part.what(),
            "The part Index/Tables/Tile.iwa of the document is damaged: a chunk is cut short."
        );
        // Long library errors are cut.
        let long = damaged("x", "é".repeat(1000));
        let NumbersError::Damaged { detail, .. } = &long else {
            unreachable!()
        };
        assert_eq!(detail.chars().count(), MAX_DETAIL + 1);
    }
}
