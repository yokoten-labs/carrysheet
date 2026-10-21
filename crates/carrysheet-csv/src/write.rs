//! Writing CSV: quoting, formula escaping, encodings.

use std::io::Write;

use encoding_rs::{Encoding, UTF_8};

use crate::LineEnding;
use crate::cell::number_shape;
use crate::encoding::{EncodeError, encode};
use crate::error::CsvError;
use crate::read::{Detected, delimiter_byte};

/// How to write a file.
///
/// The default is what Excel on Windows opens without asking: comma, UTF-8
/// with a byte order mark, CRLF, formula escaping on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteOptions {
    /// The delimiter: one ASCII character other than `"`, CR or LF.
    pub delimiter: char,
    /// The output encoding. Characters it cannot store are an error, never
    /// replaced.
    pub encoding: &'static Encoding,
    /// Start with a byte order mark. Only UTF-8 and UTF-16 have one.
    pub bom: bool,
    /// The line ending after every row, including the last.
    pub line_ending: LineEnding,
    /// Escape fields that a spreadsheet would run as a formula. See the
    /// crate documentation on formula injection.
    pub escape_formulas: bool,
    /// Start with an Excel `sep=X` line naming the delimiter.
    pub sep_line: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            delimiter: ',',
            encoding: UTF_8,
            bom: true,
            line_ending: LineEnding::CrLf,
            escape_formulas: true,
            sep_line: false,
        }
    }
}

impl WriteOptions {
    /// Options that save a file the way [`read`](crate::read) found it: same
    /// encoding, byte order mark, delimiter, `sep=` line and line ending
    /// (CRLF when the file had a single line). Formula escaping stays on.
    pub fn matching(detected: &Detected) -> Self {
        Self {
            delimiter: detected.delimiter,
            encoding: detected.encoding,
            bom: detected.bom,
            line_ending: detected.line_ending.unwrap_or(LineEnding::CrLf),
            escape_formulas: true,
            sep_line: detected.sep_line,
        }
    }
}

/// The characters that make a spreadsheet read a field as a formula
/// (OWASP, "CSV Injection").
const FORMULA_STARTS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Writes rows as CSV bytes.
///
/// `rows` can be `Vec<Vec<String>>`, `&[&[&str]]` or anything else that is a
/// slice of slices of strings.
///
/// A field is quoted when it holds the delimiter, a double quote, CR or LF,
/// when it is the only field of its row and empty (so the row does not read
/// back as a blank line), when it is escaped as a formula, and when it is the
/// first field and starts with U+FEFF or `sep=` (so it does not read back as a
/// byte order mark or a `sep=` line). Reading the output back gives the same
/// rows, except for the apostrophes added by formula escaping.
///
/// # Errors
///
/// [`CsvError::Unencodable`] names the first cell with a character the
/// encoding cannot store. [`CsvError::BomNotAvailable`],
/// [`CsvError::CannotWriteEncoding`] and [`CsvError::InvalidDelimiter`]
/// report unusable options.
pub fn write<R, F>(rows: &[R], options: &WriteOptions) -> Result<Vec<u8>, CsvError>
where
    R: AsRef<[F]>,
    F: AsRef<str>,
{
    let delimiter = delimiter_byte(options.delimiter)?;
    let eol = options.line_ending.as_str();
    let mut text = String::new();
    if options.sep_line {
        text.push_str("sep=");
        text.push(char::from(delimiter));
        text.push_str(eol);
    }
    let mut row_starts = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        row_starts.push(text.len());
        write_row(
            &mut text,
            index == 0,
            row.as_ref(),
            delimiter,
            options.escape_formulas,
        );
        text.push_str(eol);
    }
    match encode(&text, options.encoding, options.bom) {
        Ok(bytes) => Ok(bytes),
        Err(EncodeError::Other(e)) => Err(e),
        Err(EncodeError::Unmappable { character, offset }) => {
            let (row, column) = locate(rows, &row_starts, offset, delimiter, options);
            Err(CsvError::Unencodable {
                character,
                encoding: options.encoding.name(),
                row,
                column,
            })
        }
    }
}

/// Writes rows as CSV to a stream. See [`write()`].
///
/// # Errors
///
/// As [`write()`], plus [`CsvError::Io`] when the stream fails.
pub fn write_to<W, R, F>(mut writer: W, rows: &[R], options: &WriteOptions) -> Result<(), CsvError>
where
    W: Write,
    R: AsRef<[F]>,
    F: AsRef<str>,
{
    let bytes = write(rows, options)?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

fn write_row<F: AsRef<str>>(
    out: &mut String,
    first_row: bool,
    fields: &[F],
    delimiter: u8,
    escape_formulas: bool,
) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(char::from(delimiter));
        }
        let place = Place {
            first_in_file: first_row && index == 0,
            alone: fields.len() == 1,
        };
        write_field(out, field.as_ref(), place, delimiter, escape_formulas);
    }
}

/// Where a field sits, for the quoting rules that depend on it.
#[derive(Clone, Copy)]
struct Place {
    /// The first field of the first row.
    first_in_file: bool,
    /// The only field of its row.
    alone: bool,
}

fn write_field(out: &mut String, field: &str, place: Place, delimiter: u8, escape_formulas: bool) {
    let escape = escape_formulas && needs_formula_escape(field);
    let quote = escape
        || field
            .bytes()
            .any(|b| b == delimiter || matches!(b, b'"' | b'\r' | b'\n'))
        || (place.alone && field.is_empty())
        || (place.first_in_file
            && (field.starts_with('\u{FEFF}')
                || field
                    .get(..4)
                    .is_some_and(|p| p.eq_ignore_ascii_case("sep="))));
    if !quote {
        out.push_str(field);
        return;
    }
    out.push('"');
    if escape {
        out.push('\'');
    }
    for (i, part) in field.split('"').enumerate() {
        if i > 0 {
            out.push_str("\"\"");
        }
        out.push_str(part);
    }
    out.push('"');
}

/// A field starting with a formula character, unless it is a plain number
/// such as `-12` or `+3.5`, which a spreadsheet reads as that number.
fn needs_formula_escape(field: &str) -> bool {
    field.starts_with(FORMULA_STARTS) && number_shape(field).is_none()
}

/// Finds the 1-based row and column of the cell holding byte `offset` of the
/// written text.
fn locate<R, F>(
    rows: &[R],
    row_starts: &[usize],
    offset: usize,
    delimiter: u8,
    options: &WriteOptions,
) -> (usize, usize)
where
    R: AsRef<[F]>,
    F: AsRef<str>,
{
    let index = row_starts
        .partition_point(|&start| start <= offset)
        .saturating_sub(1);
    let Some(row) = rows.get(index) else {
        return (index + 1, 1);
    };
    let mut position = row_starts.get(index).copied().unwrap_or_default();
    let fields = row.as_ref();
    let mut scratch = String::new();
    for (column, field) in fields.iter().enumerate() {
        scratch.clear();
        let place = Place {
            first_in_file: index == 0 && column == 0,
            alone: fields.len() == 1,
        };
        write_field(
            &mut scratch,
            field.as_ref(),
            place,
            delimiter,
            options.escape_formulas,
        );
        if offset < position + scratch.len() {
            return (index + 1, column + 1);
        }
        position += scratch.len() + 1;
    }
    (index + 1, fields.len().max(1))
}
