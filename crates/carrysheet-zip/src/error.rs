//! Errors. Their texts come only from this crate, never from the file.

use std::fmt;

/// Why an archive or entry could not be read.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Reading the underlying file failed.
    Io(std::io::Error),
    /// The archive breaks the format, or was built to mislead: what is
    /// wrong, without any text from the file.
    Damaged(String),
    /// An entry's deflate data is shaped to cost time rather than to hold
    /// data, such as runs of empty blocks: what was seen, without any text
    /// from the file. A valid but unusual writer can cause some of these.
    Slow(String),
    /// A [`Limits`](crate::Limits) value would be passed.
    LimitExceeded {
        /// What was counted, such as "zip entries".
        what: &'static str,
        /// The limit.
        max: u64,
    },
    /// Valid zip, but not something this reader handles, such as a
    /// compression method other than Stored and Deflate, encryption, or
    /// an archive split over several disks.
    Unsupported(String),
    /// The caller's interrupt (see
    /// [`Archive::set_interrupt`](crate::Archive::set_interrupt)) asked
    /// to stop, such as when a time limit ran out.
    Interrupted,
}

/// Texts of the [`Error::Slow`] refusals.
pub(crate) const SLOW_SHAPE: &str = "compressed data is far larger than its output";
pub(crate) const SLOW_NO_OUTPUT: &str = "an entry's deflate data consumes input without producing output (such as runs of empty blocks)";
pub(crate) const SLOW_IDLE_RUN: &str = "an entry's deflate data has a long run of input without output between pieces of output (such as empty blocks)";

impl Error {
    /// True when an entry was refused because its deflate data is shaped
    /// to cost time rather than to hold data ([`Error::Slow`]). Callers
    /// can say so instead of calling the file damaged.
    pub fn is_slow_to_unpack(&self) -> bool {
        matches!(self, Error::Slow(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "reading the zip failed: {e}"),
            Error::Damaged(what) => write!(f, "the zip is damaged: {what}"),
            Error::Slow(what) => write!(f, "the zip unpacks too slowly: {what}"),
            Error::LimitExceeded { what, max } => {
                write!(f, "the zip goes over a limit: {what} (at most {max})")
            }
            Error::Unsupported(what) => write!(f, "the zip cannot be read: {what}"),
            Error::Interrupted => write!(f, "reading the zip was stopped by the caller"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        // A read past the end means a size or offset in the file lies.
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Damaged("an offset or size points past the end of the file".into())
        } else {
            Error::Io(e)
        }
    }
}

pub(crate) fn damaged(what: impl Into<String>) -> Error {
    Error::Damaged(what.into())
}

pub(crate) fn limit(what: &'static str, max: u64) -> Error {
    Error::LimitExceeded { what, max }
}
