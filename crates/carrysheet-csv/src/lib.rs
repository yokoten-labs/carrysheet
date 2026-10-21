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
//! # Regional conventions
//!
//! [`read_values`] reads typed [`Value`]s (numbers, dates, times, booleans)
//! under a [`Convention`]: delimiter, decimal mark, digit grouping, date
//! order and two-digit year window. What the caller leaves open in a
//! [`ConventionChoice`] is detected per column from a bounded sample (the
//! first 1,000 rows, 1 MiB of text, 200,000 cells), and the [`Analysis`]
//! says what was decided and why in short plain English. When the data
//! cannot settle a question, such as `03/04/2026` with no date that has a
//! day over 12, the values stay text and a [`Note`] says so. A column
//! borrows the decision of the other columns of its kind only when they
//! cast at least ten votes, and every value read that way is counted in a
//! note; text columns never vote.
//!
//! Some values always stay text, with a note: leading zeros (`007`, ZIP
//! codes), more than 15 significant digits (IDs, card numbers; 16 or 17
//! only when they are a computed value's exact shortest form), a plus sign
//! before eight or more digits that are not in accepted thousands groups
//! (phone numbers; not applied when the caller chose the decimal mark and
//! grouping, so a convention's own `+12345678` reads back), subnormal
//! values that would round, values like `3/4` or `1-2` and gene names like
//! `SEPT2` that Excel turns into dates. Whole numbers grouped with plain
//! spaces (`1 234`) are numbers only when the space grouping is chosen or
//! seen with a fraction (`1 234,5`), so `640 480` stays text, with a note
//! in a decimal-comma file; with no-break spaces (`2 102 650`) a decided
//! decimal comma is enough.
//! Values like `1E5` are numbers, as in Excel, with a note that they may be
//! codes.
//!
//! [`write_values`] writes typed rows in any convention, for example `;`
//! and decimal comma for Germany ([`Convention::GERMANY`]).
//!
//! ```
//! use carrysheet_csv::{Convention, ConventionChoice, ReadOptions, WriteOptions, read_values, write_values};
//!
//! let us = "Item,Price,Date\nTea,\"1,234.50\",10/20/2026\nCoffee,3.99,12/31/2026\n";
//! let r = read_values(us.as_bytes(), &ReadOptions::default(), &ConventionChoice::default())?;
//! let de = Convention::GERMANY;
//! let out = write_values(&r.values, &de, &WriteOptions { bom: false, ..WriteOptions::for_convention(&de) })?;
//! assert_eq!(out, "Item;Price;Date\r\nTea;1.234,50;20.10.2026\r\nCoffee;3,99;31.12.2026\r\n".as_bytes());
//! # Ok::<(), carrysheet_csv::CsvError>(())
//! ```
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
mod regional;
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
pub use regional::{
    Analysis, ColumnKind, ColumnReport, Convention, ConventionChoice, Currency, Date, DateOrder,
    DecimalMark, Decision, EXCEL_TWO_DIGIT_PIVOT, Grouping, Header, NegativeStyle, Note, NoteKind,
    Number, NumberFormat, Time, Value, ValuesResult, read_values, write_values,
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
