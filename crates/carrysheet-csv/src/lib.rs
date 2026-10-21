//! Read and write CSV and TSV files in legacy encodings, safely.
//!
//! ```
//! use carrysheet_csv::{ReadOptions, WriteOptions, read, write};
//!
//! let bytes = b"name;city\r\nAda;\"Z\xc3\xbcrich\"\r\n";
//! let file = read(bytes, &ReadOptions::default())?;
//! assert_eq!(file.rows, [["name", "city"], ["Ada", "Zürich"]]);
//! assert_eq!(file.detected.delimiter, ';');
//!
//! // Save it the way it came.
//! let out = write(&file.rows, &WriteOptions::matching(&file.detected))?;
//! assert_eq!(out, b"name;city\r\nAda;Z\xc3\xbcrich\r\n");
//! # Ok::<(), carrysheet_csv::CsvError>(())
//! ```
//!
//! # Rows are text
//!
//! [`read`] returns every field as a `String`, exactly as written in the file
//! (quotes removed, `""` turned into `"`, spaces kept). Nothing is lost:
//! `007` stays `007`, `1.50` stays `1.50`, and writing the rows back gives
//! the same rows. When you want numbers and booleans, call [`Cell::infer`]
//! on a field. Its rules keep text whenever a number would change what you
//! see (leading zeros, more than 15 digits).
//!
//! # Encodings
//!
//! [`detect_encoding`] recognizes a byte order mark (UTF-8, UTF-16 LE/BE),
//! valid UTF-8, and otherwise asks `chardetng`, the detector Firefox uses,
//! which covers GBK/GB18030, Big5, Shift_JIS, EUC-KR, Windows-1252 and the
//! other legacy encodings of the Web. It returns a [`Confidence`]; when it is
//! below [`Confidence::High`], show the guess and let the person choose.
//! Set [`ReadOptions::encoding`] to skip detection.
//!
//! Bytes that are not valid in the encoding are an error by default
//! ([`Malformed::Error`]): replacing them with U+FFFD and saving would lose
//! the original bytes. [`Malformed::Replace`] reads them anyway and counts
//! them.
//!
//! On write, a character the output encoding cannot store is an error
//! naming its row and column ([`CsvError::Unencodable`]), never a silent
//! `?`. UTF-8 and UTF-16 can store everything, and so can GB18030.
//!
//! # Delimiters and quoting
//!
//! [`detect_delimiter`] chooses among comma, semicolon, tab and pipe by how
//! consistently each one splits the first rows. An Excel `sep=;` first line
//! names the delimiter and is not part of the rows. Quoted fields may hold
//! delimiters, line breaks and doubled quotes. Rows may end in CRLF, LF or a
//! lone CR, and the last row needs no line ending.
//!
//! # Formula injection
//!
//! A spreadsheet runs a field that starts with `=`, `+`, `-` or `@` as a
//! formula, and tab or CR at the start can hide such a field. A CSV built
//! from untrusted data can then run commands or leak data when someone opens
//! it (OWASP, "CSV Injection"). With [`WriteOptions::escape_formulas`] on
//! (the default), [`write()`] follows the OWASP advice: it puts an apostrophe
//! before such a field and wraps it in double quotes, so `=1+2` is written as
//! `"'=1+2"` and the spreadsheet shows it as text. Plain numbers such as
//! `-12` or `+3.5` are left alone, because a spreadsheet reads them as that
//! number and nothing more; escaping them would turn every negative number
//! into text. Reading such a file back gives `'=1+2`, with the apostrophe.
//! Turn escaping off for files that only programs read, or when the formulas
//! are meant.
//!
//! # Hostile input
//!
//! Every input is treated as hostile. [`Limits`] caps total bytes, rows,
//! cells per row, cells in total and bytes per cell, and each limit is
//! checked before the data it guards is kept. Errors say what happened, why
//! and how to fix it, and never quote cell contents. Nothing panics.

mod cell;
mod encoding;
mod error;
mod read;
mod write;

pub use cell::Cell;
pub use encoding::{Confidence, EncodingGuess, detect_encoding, encoding_for_label};
pub use encoding_rs::{
    BIG5, EUC_KR, Encoding, GB18030, GBK, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252,
};
pub use error::CsvError;
pub use read::{
    DELIMITER_CANDIDATES, Detected, Limits, Malformed, ReadOptions, ReadResult, detect_delimiter,
    read, read_from,
};
pub use write::{WriteOptions, write, write_to};

/// The bytes that end a row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// `\r\n`, as Windows and RFC 4180 use. The default.
    #[default]
    CrLf,
    /// `\n`, as Linux and macOS use.
    Lf,
    /// `\r` alone, as classic Mac OS used.
    Cr,
}

impl LineEnding {
    /// The line ending as text.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::CrLf => "\r\n",
            LineEnding::Lf => "\n",
            LineEnding::Cr => "\r",
        }
    }
}
