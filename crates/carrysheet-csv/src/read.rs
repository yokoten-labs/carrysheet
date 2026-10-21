//! Reading CSV: limits, the parser, delimiter detection.

use std::io::Read;

use encoding_rs::Encoding;

use crate::LineEnding;
use crate::encoding::{Confidence, decode, detect_encoding};
use crate::error::CsvError;

/// Limits that keep a damaged or hostile file from using up memory.
///
/// The defaults fit a large real file in a few gigabytes of memory at most.
/// Every limit is checked before the data it guards is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes of input. Default 256 MiB.
    pub max_total_bytes: usize,
    /// The most rows. Default 1,048,576, the row limit of Excel.
    pub max_rows: usize,
    /// The most cells in one row. Default 16,384, the column limit of Excel.
    pub max_columns: usize,
    /// The most cells in the whole file. Default 10,000,000. Each cell costs
    /// some bytes of bookkeeping besides its text, so this bounds memory for
    /// files made of many tiny cells.
    pub max_cells: usize,
    /// The most bytes of UTF-8 text in one cell. Default 1 MiB.
    pub max_field_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total_bytes: 256 << 20,
            max_rows: 1 << 20,
            max_columns: 1 << 14,
            max_cells: 10_000_000,
            max_field_bytes: 1 << 20,
        }
    }
}

/// What to do with bytes that are not valid in the encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Malformed {
    /// Stop with [`CsvError::MalformedText`]. The default, because saving
    /// replaced text would lose the original bytes for good.
    #[default]
    Error,
    /// Replace each bad sequence with U+FFFD and count it in
    /// [`Detected::replaced`].
    Replace,
}

/// How to read a file. `ReadOptions::default()` detects everything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadOptions {
    /// The encoding, or `None` to detect it with [`detect_encoding`].
    pub encoding: Option<&'static Encoding>,
    /// The delimiter, or `None` to use a `sep=` first line or detect it with
    /// [`detect_delimiter`].
    pub delimiter: Option<char>,
    /// What to do with bytes that are not valid in the encoding.
    pub malformed: Malformed,
    /// Size and count limits.
    pub limits: Limits,
}

/// What [`read`] found out about the file. Pass it to
/// [`WriteOptions::matching`](crate::WriteOptions::matching) to save the file
/// the way it came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detected {
    /// The encoding the file was read with.
    pub encoding: &'static Encoding,
    /// How sure the encoding is; [`Confidence::Certain`] when the caller chose it.
    pub encoding_confidence: Confidence,
    /// `true` when the encoding was detected, `false` when the caller chose it.
    pub encoding_detected: bool,
    /// The file starts with a byte order mark.
    pub bom: bool,
    /// The delimiter the file was read with.
    pub delimiter: char,
    /// `true` when the delimiter was guessed, `false` when the caller chose
    /// it or the file named it in a `sep=` line.
    pub delimiter_detected: bool,
    /// The file starts with an Excel `sep=` line. It is not part of the rows.
    pub sep_line: bool,
    /// The delimiter was guessed as `;` over a `,` that splits the rows as
    /// well, because the `;` cells hold decimal-comma numbers (`1,5;2,5`).
    /// See [`detect_delimiter`].
    pub semicolon_for_decimal_commas: bool,
    /// The first line ending outside quotes, or `None` for a single line.
    pub line_ending: Option<LineEnding>,
    /// How many bad byte sequences became U+FFFD (only with
    /// [`Malformed::Replace`]).
    pub replaced: usize,
}

/// Rows read from a file, and what was found out about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadResult {
    /// The rows, each a list of fields as text. A blank line is an empty row.
    pub rows: Vec<Vec<String>>,
    /// The encoding, delimiter and other facts about the file.
    pub detected: Detected,
}

/// The delimiters [`detect_delimiter`] tries, in order of preference on a tie.
pub const DELIMITER_CANDIDATES: [char; 4] = [',', ';', '\t', '|'];

/// How many rows delimiter detection looks at.
const DETECT_ROWS: usize = 200;

/// How many bytes of text delimiter detection looks at, so its cost does
/// not grow with a huge first field or row.
const DETECT_BYTES: usize = 1 << 20;

/// Reads CSV from bytes.
///
/// # Errors
///
/// Returns a [`CsvError`] when a limit is crossed, a quote never closes, the
/// bytes are not valid in the encoding (with [`Malformed::Error`]) or the
/// delimiter is not usable. It never panics.
pub fn read(bytes: &[u8], options: &ReadOptions) -> Result<ReadResult, CsvError> {
    let limits = options.limits;
    if bytes.len() > limits.max_total_bytes {
        return Err(CsvError::InputTooLarge {
            limit: limits.max_total_bytes,
        });
    }
    let chosen_delimiter = options.delimiter.map(delimiter_byte).transpose()?;

    let guess = match options.encoding {
        Some(encoding) => {
            let bom_len = match Encoding::for_bom(bytes) {
                Some((found, len)) if found == encoding => len,
                _ => 0,
            };
            crate::EncodingGuess {
                encoding,
                confidence: Confidence::Certain,
                bom_len,
            }
        }
        None => detect_encoding(bytes),
    };
    let body = bytes.get(guess.bom_len..).unwrap_or_default();
    let (text, replaced) = decode(
        body,
        guess.encoding,
        options.malformed == Malformed::Replace,
    )?;

    let (sep, data) = split_sep_line(&text);
    let (delimiter, delimiter_detected, semicolon_for_decimal_commas) =
        match (chosen_delimiter, sep) {
            (Some(d), _) | (None, Some(d)) => (d, false, false),
            (None, None) => {
                let found = detect_delimiter_with(data, &limits);
                (
                    found.delimiter.unwrap_or(b','),
                    true,
                    found.by_decimal_commas,
                )
            }
        };

    let first_line = if sep.is_some() { 2 } else { 1 };
    let mut parser = Parser::new(data, delimiter, limits, first_line);
    let mut rows = Vec::new();
    while let Some(row) = parser.next_row()? {
        rows.push(row);
    }

    Ok(ReadResult {
        rows,
        detected: Detected {
            encoding: guess.encoding,
            encoding_confidence: guess.confidence,
            encoding_detected: options.encoding.is_none(),
            bom: guess.bom_len > 0,
            delimiter: char::from(delimiter),
            delimiter_detected,
            sep_line: sep.is_some(),
            semicolon_for_decimal_commas,
            line_ending: parser.line_ending,
            replaced,
        },
    })
}

/// Reads CSV from a stream, stopping as soon as it passes
/// [`Limits::max_total_bytes`].
///
/// # Errors
///
/// As [`read`], plus [`CsvError::Io`] when the stream fails.
pub fn read_from<R: Read>(reader: R, options: &ReadOptions) -> Result<ReadResult, CsvError> {
    let limit = options.limits.max_total_bytes;
    let mut bytes = Vec::new();
    let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    reader.take(cap).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(CsvError::InputTooLarge { limit });
    }
    read(&bytes, options)
}

/// Guesses the delimiter of decoded CSV text among comma, semicolon, tab and
/// pipe.
///
/// It reads the first 200 rows (and at most the first 1 MiB) with each
/// candidate, quotes included, and picks the one whose most common cell
/// count (at least 2) covers the most rows. When `;` and `,` split the rows
/// equally well, `;` wins if its cells hold decimal-comma numbers (`1,5`)
/// and the `,` cells hold semicolons, as in `1,5;2,5` or `a;1,5`, even when
/// `,` gives more cells. Other ties go to the larger cell count, then to the
/// order of [`DELIMITER_CANDIDATES`], so `id,name;alias` splits on `,`.
/// Returns `None` when no candidate splits any row, for example in a file
/// with one column; a comma is then a fine default.
pub fn detect_delimiter(text: &str) -> Option<char> {
    detect_delimiter_with(text, &Limits::default())
        .delimiter
        .map(char::from)
}

/// At most the first `max` bytes of `text`, cut after a line feed or a
/// lone carriage return (old Mac files) when the text is longer, so the
/// last row of the sample is whole.
fn detection_sample(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = text.get(..end).unwrap_or_default();
    match head.rfind(['\n', '\r']) {
        Some(at) => head.get(..=at).unwrap_or(head),
        None => head,
    }
}

/// The delimiter found, and whether `;` won a tie with `,` on decimal-comma
/// evidence.
struct Found {
    delimiter: Option<u8>,
    by_decimal_commas: bool,
}

fn detect_delimiter_with(text: &str, limits: &Limits) -> Found {
    let text = detection_sample(text, DETECT_BYTES);
    // (rows with the common count, rows, common count, delimiter)
    let mut best: Option<(u64, u64, usize, u8)> = None;
    let mut by_decimal_commas = false;
    // Some `,` cell holds a `;`.
    let mut comma_cells_have_semicolons = false;
    for candidate in DELIMITER_CANDIDATES {
        let delimiter = candidate as u8;
        let mut parser = Parser::new(text, delimiter, *limits, 1);
        let mut counts: Vec<usize> = Vec::new();
        let mut cells = CellEvidence::default();
        while counts.len() < DETECT_ROWS {
            match parser.next_row() {
                Ok(Some(row)) if row.is_empty() => {}
                Ok(Some(row)) => {
                    cells.add(&row);
                    counts.push(row.len());
                }
                Ok(None) | Err(_) => break,
            }
        }
        if delimiter == b',' {
            comma_cells_have_semicolons = cells.semicolons > 0;
        }
        let Some((common, hits)) = most_common(&counts) else {
            continue;
        };
        if common < 2 {
            continue;
        }
        let rows = counts.len() as u64;
        let (better, tie) = match best {
            None => (true, false),
            Some((best_hits, best_rows, best_common, best_delimiter)) => {
                let (a, b) = (hits * best_rows, best_hits * rows);
                let decimal_commas = delimiter == b';'
                    && best_delimiter == b','
                    && comma_cells_have_semicolons
                    && cells.decimal_commas > cells.other_commas;
                let plain = a > b || (a == b && common > best_common);
                (
                    plain || (a == b && decimal_commas),
                    !plain && a == b && decimal_commas,
                )
            }
        };
        if better {
            best = Some((hits, rows, common, delimiter));
            by_decimal_commas = tie;
        }
    }
    Found {
        delimiter: best.map(|(_, _, _, delimiter)| delimiter),
        by_decimal_commas,
    }
}

/// What the cells of one delimiter candidate hold, for telling `;` files
/// with decimal commas from `,` files.
#[derive(Default)]
struct CellEvidence {
    /// Cells that are decimal-comma numbers: `1,5`, `-1.234,50`.
    decimal_commas: usize,
    /// Other cells with a comma.
    other_commas: usize,
    /// Cells with a semicolon.
    semicolons: usize,
}

impl CellEvidence {
    fn add(&mut self, row: &[String]) {
        for cell in row {
            if cell.contains(';') {
                self.semicolons += 1;
            }
            if cell.contains(',') {
                if is_decimal_comma(cell.trim()) {
                    self.decimal_commas += 1;
                } else {
                    self.other_commas += 1;
                }
            }
        }
    }
}

/// `[+-]digits(.digits)*,digits`, as in `1,5` or `-1.234,50`.
fn is_decimal_comma(cell: &str) -> bool {
    let cell = cell.strip_prefix(['+', '-']).unwrap_or(cell);
    let Some((int, frac)) = cell.split_once(',') else {
        return false;
    };
    !int.is_empty()
        && !frac.is_empty()
        && frac.bytes().all(|b| b.is_ascii_digit())
        && int
            .split('.')
            .all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()))
}

/// The most frequent value and how often it occurs; ties go to the larger value.
fn most_common(values: &[usize]) -> Option<(usize, u64)> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mut best: Option<(usize, u64)> = None;
    for chunk in sorted.chunk_by(|a, b| a == b) {
        let Some(&value) = chunk.first() else {
            continue;
        };
        let count = chunk.len() as u64;
        if best.is_none_or(|(_, c)| count >= c) {
            best = Some((value, count));
        }
    }
    best
}

/// Checks that `c` can separate cells and returns it as a byte.
pub(crate) fn delimiter_byte(c: char) -> Result<u8, CsvError> {
    match u8::try_from(c) {
        Ok(b) if b.is_ascii() && !matches!(b, b'"' | b'\r' | b'\n') => Ok(b),
        _ => Err(CsvError::InvalidDelimiter { delimiter: c }),
    }
}

/// Splits off an Excel `sep=X` first line (letter case ignored).
fn split_sep_line(text: &str) -> (Option<u8>, &str) {
    let b = text.as_bytes();
    let Some(prefix) = b.get(..4) else {
        return (None, text);
    };
    if !prefix.eq_ignore_ascii_case(b"sep=") {
        return (None, text);
    }
    let Some(&d) = b.get(4) else {
        return (None, text);
    };
    if delimiter_byte(char::from(d)).is_err() {
        return (None, text);
    }
    let rest_at = match (b.get(5), b.get(6)) {
        (None, _) => 5,
        (Some(b'\r'), Some(b'\n')) => 7,
        (Some(b'\r' | b'\n'), _) => 6,
        _ => return (None, text),
    };
    (Some(d), text.get(rest_at..).unwrap_or_default())
}

/// A row-at-a-time CSV parser over decoded text.
///
/// Rules: rows end at CRLF, LF or a lone CR; a final line without a line
/// ending is a row; a blank line is an empty row. A field that starts with a
/// double quote runs to the next lone double quote, and `""` inside it is one
/// quote; text after the closing quote up to the next delimiter is kept as
/// is. A double quote inside an unquoted field is plain text. Spaces are
/// kept.
pub(crate) struct Parser<'a> {
    text: &'a str,
    pos: usize,
    line: u64,
    rows: usize,
    cells: usize,
    delimiter: u8,
    limits: Limits,
    line_ending: Option<LineEnding>,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(text: &'a str, delimiter: u8, limits: Limits, first_line: u64) -> Self {
        Self {
            text,
            pos: 0,
            line: first_line,
            rows: 0,
            cells: 0,
            delimiter,
            limits,
            line_ending: None,
        }
    }

    /// Text between two byte offsets. The parser only cuts next to ASCII
    /// bytes, which are always character boundaries.
    fn slice(&self, start: usize, end: usize) -> &'a str {
        self.text.get(start..end).unwrap_or_default()
    }

    fn is_field_end(&self, b: u8) -> bool {
        b == self.delimiter || b == b'\r' || b == b'\n'
    }

    pub(crate) fn next_row(&mut self) -> Result<Option<Vec<String>>, CsvError> {
        let bytes = self.text.as_bytes();
        let Some(&first) = bytes.get(self.pos) else {
            return Ok(None);
        };
        let row = self.rows + 1;
        let row_line = self.line;
        if self.rows >= self.limits.max_rows {
            return Err(CsvError::TooManyRows {
                limit: self.limits.max_rows,
                line: row_line,
            });
        }
        let mut fields = Vec::new();
        if first == b'\r' || first == b'\n' {
            self.end_line();
            self.rows += 1;
            return Ok(Some(fields));
        }
        loop {
            if fields.len() >= self.limits.max_columns {
                return Err(CsvError::TooManyColumns {
                    limit: self.limits.max_columns,
                    row,
                    line: row_line,
                });
            }
            if self.cells >= self.limits.max_cells {
                return Err(CsvError::TooManyCells {
                    limit: self.limits.max_cells,
                    row,
                    line: row_line,
                });
            }
            let field = self.field(row, fields.len() + 1)?;
            if field.len() > self.limits.max_field_bytes {
                return Err(CsvError::FieldTooLarge {
                    limit: self.limits.max_field_bytes,
                    row,
                    column: fields.len() + 1,
                    line: row_line,
                });
            }
            self.cells += 1;
            fields.push(field);
            match bytes.get(self.pos) {
                Some(&b) if b == self.delimiter => self.pos += 1,
                Some(b'\r' | b'\n') => {
                    self.end_line();
                    break;
                }
                _ => break,
            }
        }
        self.rows += 1;
        Ok(Some(fields))
    }

    /// Consumes one line ending at `pos`.
    fn end_line(&mut self) {
        let bytes = self.text.as_bytes();
        let ending = match (bytes.get(self.pos), bytes.get(self.pos + 1)) {
            (Some(b'\r'), Some(b'\n')) => LineEnding::CrLf,
            (Some(b'\r'), _) => LineEnding::Cr,
            _ => LineEnding::Lf,
        };
        self.pos += if ending == LineEnding::CrLf { 2 } else { 1 };
        self.line += 1;
        self.line_ending.get_or_insert(ending);
    }

    /// Reads one field, leaving `pos` on the delimiter, line ending or end.
    fn field(&mut self, row: usize, column: usize) -> Result<String, CsvError> {
        let bytes = self.text.as_bytes();
        let start = self.pos;
        if bytes.get(start) != Some(&b'"') {
            let mut i = start;
            while bytes.get(i).is_some_and(|&b| !self.is_field_end(b)) {
                i += 1;
            }
            self.pos = i;
            return Ok(self.slice(start, i).to_owned());
        }

        let open_line = self.line;
        let mut out = String::new();
        let mut segment = start + 1;
        let mut i = segment;
        loop {
            match bytes.get(i) {
                None => {
                    return Err(CsvError::UnclosedQuote {
                        row,
                        line: open_line,
                    });
                }
                Some(b'"') => {
                    out.push_str(self.slice(segment, i));
                    if bytes.get(i + 1) == Some(&b'"') {
                        out.push('"');
                        i += 2;
                        segment = i;
                    } else {
                        i += 1;
                        break;
                    }
                }
                Some(b'\n') => {
                    self.line += 1;
                    i += 1;
                }
                Some(b'\r') => {
                    if bytes.get(i + 1) != Some(&b'\n') {
                        self.line += 1;
                    }
                    i += 1;
                }
                Some(_) => i += 1,
            }
            if i - start
                > self
                    .limits
                    .max_field_bytes
                    .saturating_mul(2)
                    .saturating_add(2)
            {
                // Even with every quote doubled the field is too large; stop
                // before copying more of it.
                return Err(CsvError::FieldTooLarge {
                    limit: self.limits.max_field_bytes,
                    row,
                    column,
                    line: open_line,
                });
            }
        }
        let tail = i;
        while bytes.get(i).is_some_and(|&b| !self.is_field_end(b)) {
            i += 1;
        }
        out.push_str(self.slice(tail, i));
        self.pos = i;
        Ok(out)
    }
}
