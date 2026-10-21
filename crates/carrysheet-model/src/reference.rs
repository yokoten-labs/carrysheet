//! A1 references: cells (`B7`, `$B$7`), ranges (`A1:C10`, `A:C`, `2:5`) and
//! sheet-qualified ranges (`Sheet1!A1:C10`, `'Q1 売上'!B2`).
//!
//! Rows and columns are stored 0-based and shown 1-based. Parsing is lenient
//! where Excel is (lower-case column letters, unquoted non-ASCII sheet names)
//! and formatting is strict (upper-case letters, sheet names quoted whenever
//! there is any doubt).

use std::fmt;
use std::str::FromStr;

/// Rows in a sheet (Excel 2007 and later).
pub const MAX_ROWS: u32 = 1_048_576;
/// Columns in a sheet, `A` to `XFD`.
pub const MAX_COLS: u32 = 16_384;
/// Longest sheet name Excel accepts, in UTF-16 code units.
pub const MAX_SHEET_NAME_LEN: usize = 31;

/// Characters Excel does not allow in a sheet name.
const FORBIDDEN_SHEET_CHARS: &[char] = &[':', '\\', '/', '?', '*', '[', ']'];

/// Why a reference or sheet name could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefError {
    /// The text was empty.
    Empty,
    /// The text is not a reference; `reason` says what is wrong.
    Invalid { input: String, reason: &'static str },
    /// The row is 0 or above [`MAX_ROWS`].
    RowOutOfRange { input: String },
    /// The column is past `XFD` ([`MAX_COLS`]).
    ColumnOutOfRange { input: String },
    /// The sheet name breaks one of Excel's rules.
    InvalidSheetName { name: String, reason: &'static str },
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefError::Empty => write!(f, "the reference is empty"),
            RefError::Invalid { input, reason } => {
                write!(f, "\"{input}\" is not a valid reference: {reason}")
            }
            RefError::RowOutOfRange { input } => write!(
                f,
                "\"{input}\" has a row outside 1 to {MAX_ROWS}, the rows a sheet can have"
            ),
            RefError::ColumnOutOfRange { input } => write!(
                f,
                "\"{input}\" has a column past XFD, the last of the {MAX_COLS} columns"
            ),
            RefError::InvalidSheetName { name, reason } => {
                write!(f, "\"{name}\" is not a valid sheet name: {reason}")
            }
        }
    }
}

impl std::error::Error for RefError {}

fn invalid(input: &str, reason: &'static str) -> RefError {
    RefError::Invalid {
        input: input.to_owned(),
        reason,
    }
}

/// Turns a 0-based column index into letters: 0 is `A`, 26 is `AA`.
///
/// Indexes past the last column still produce letters; callers that need a
/// valid column check against [`MAX_COLS`] first.
pub fn column_to_letters(col: u32) -> String {
    let mut n = u64::from(col) + 1;
    let mut letters = Vec::with_capacity(4);
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        letters.push(b'A' + rem);
        n = (n - 1) / 26;
    }
    letters.reverse();
    String::from_utf8(letters).unwrap_or_default()
}

/// Turns column letters (any case) into a 0-based index: `A` is 0, `xfd` is 16383.
///
/// Returns `None` for empty text, anything but ASCII letters, or a column past `XFD`.
pub fn column_from_letters(letters: &str) -> Option<u32> {
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for b in letters.bytes() {
        if !b.is_ascii_alphabetic() {
            return None;
        }
        n = n * 26 + u32::from(b.to_ascii_uppercase() - b'A' + 1);
    }
    if n == 0 || n > MAX_COLS {
        None
    } else {
        Some(n - 1)
    }
}

/// Splits an optional `$` off the front.
fn strip_dollar(s: &str) -> (bool, &str) {
    match s.strip_prefix('$') {
        Some(rest) => (true, rest),
        None => (false, s),
    }
}

/// Parses 1-based row digits into a 0-based row.
fn parse_row(digits: &str, input: &str) -> Result<u32, RefError> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(input, "a row number is missing"));
    }
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() || trimmed.len() > 7 {
        return Err(RefError::RowOutOfRange {
            input: input.to_owned(),
        });
    }
    let n: u32 = trimmed.parse().map_err(|_| RefError::RowOutOfRange {
        input: input.to_owned(),
    })?;
    if n > MAX_ROWS {
        return Err(RefError::RowOutOfRange {
            input: input.to_owned(),
        });
    }
    Ok(n - 1)
}

/// Parses column letters into a 0-based column.
fn parse_col(letters: &str, input: &str) -> Result<u32, RefError> {
    if letters.is_empty() {
        return Err(invalid(input, "a column letter is missing"));
    }
    if !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(invalid(input, "columns are letters A to XFD"));
    }
    column_from_letters(letters).ok_or_else(|| RefError::ColumnOutOfRange {
        input: input.to_owned(),
    })
}

/// One cell, such as `B7` or `$B$7`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellRef {
    /// 0-based row.
    pub row: u32,
    /// 0-based column.
    pub col: u32,
    /// `$` before the row.
    pub row_absolute: bool,
    /// `$` before the column.
    pub col_absolute: bool,
}

impl CellRef {
    /// A relative reference to a 0-based row and column, checked against the sheet limits.
    pub fn new(row: u32, col: u32) -> Result<Self, RefError> {
        if row >= MAX_ROWS {
            return Err(RefError::RowOutOfRange {
                input: format!("row {}", u64::from(row) + 1),
            });
        }
        if col >= MAX_COLS {
            return Err(RefError::ColumnOutOfRange {
                input: column_to_letters(col),
            });
        }
        Ok(CellRef {
            row,
            col,
            row_absolute: false,
            col_absolute: false,
        })
    }

    /// The same cell with both `$` set.
    pub fn absolute(self) -> Self {
        CellRef {
            row_absolute: true,
            col_absolute: true,
            ..self
        }
    }

    /// The same cell without any `$`.
    pub fn relative(self) -> Self {
        CellRef {
            row_absolute: false,
            col_absolute: false,
            ..self
        }
    }

    /// True when both point at the same cell, ignoring `$`.
    pub fn same_cell(&self, other: &CellRef) -> bool {
        self.row == other.row && self.col == other.col
    }

    /// Parses `A1`, `$A1`, `A$1` or `$A$1` (column letters in any case).
    pub fn parse(input: &str) -> Result<Self, RefError> {
        if input.is_empty() {
            return Err(RefError::Empty);
        }
        if !input.is_ascii() {
            return Err(invalid(
                input,
                "a cell is column letters then a row number, like B7",
            ));
        }
        let (col_absolute, rest) = strip_dollar(input);
        let split = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let (letters, rest) = rest.split_at(split);
        let (row_absolute, digits) = strip_dollar(rest);
        if letters.is_empty() {
            return Err(invalid(input, "a column letter is missing"));
        }
        if digits.is_empty() {
            return Err(invalid(input, "a row number is missing"));
        }
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid(
                input,
                "a cell is column letters then a row number, like B7",
            ));
        }
        let col = parse_col(letters, input)?;
        let row = parse_row(digits, input)?;
        Ok(CellRef {
            row,
            col,
            row_absolute,
            col_absolute,
        })
    }
}

impl fmt::Display for CellRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.col_absolute {
            f.write_str("$")?;
        }
        f.write_str(&column_to_letters(self.col))?;
        if self.row_absolute {
            f.write_str("$")?;
        }
        write!(f, "{}", u64::from(self.row) + 1)
    }
}

impl FromStr for CellRef {
    type Err = RefError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CellRef::parse(s)
    }
}

/// How a range was written, so it is shown the same way back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeShape {
    /// `A1` or `A1:C3`.
    Cells,
    /// Whole columns, `A:C`.
    Columns,
    /// Whole rows, `2:5`.
    Rows,
}

/// A rectangle of cells. Ranges from the constructors and parsers have
/// `start` at the top-left corner and `end` at the bottom-right one; parsing
/// `B2:A1` gives `A1:B2`, as Excel does.
///
/// The fields are public, so a range built by hand may have its corners the
/// other way round. Every method here reads such a range as the rectangle
/// between its corners, never panicking; [`Range::sorted`] puts them in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Range {
    pub start: CellRef,
    pub end: CellRef,
    pub shape: RangeShape,
}

/// One end of a range as written: a cell, a column or a row.
enum Part {
    Cell(CellRef),
    Col { col: u32, absolute: bool },
    Row { row: u32, absolute: bool },
}

fn parse_part(part: &str, input: &str) -> Result<Part, RefError> {
    if part.is_empty() {
        return Err(invalid(input, "one side of the ':' is empty"));
    }
    if !part.is_ascii() {
        return Err(invalid(
            input,
            "a range is two cells joined by ':', like A1:C10",
        ));
    }
    let (absolute, rest) = strip_dollar(part);
    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Ok(Part::Col {
            col: parse_col(rest, input)?,
            absolute,
        });
    }
    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(Part::Row {
            row: parse_row(rest, input)?,
            absolute,
        });
    }
    match CellRef::parse(part) {
        Ok(cell) => Ok(Part::Cell(cell)),
        Err(RefError::Invalid { reason, .. }) => Err(invalid(input, reason)),
        Err(RefError::RowOutOfRange { .. }) => Err(RefError::RowOutOfRange {
            input: input.to_owned(),
        }),
        Err(RefError::ColumnOutOfRange { .. }) => Err(RefError::ColumnOutOfRange {
            input: input.to_owned(),
        }),
        Err(other) => Err(other),
    }
}

impl Range {
    /// A range of cells between two corners, in any order.
    pub fn new(a: CellRef, b: CellRef) -> Self {
        Self::normalized(a, b, RangeShape::Cells)
    }

    /// A range holding one cell.
    pub fn cell(cell: CellRef) -> Self {
        Range {
            start: cell,
            end: cell,
            shape: RangeShape::Cells,
        }
    }

    /// Whole columns, 0-based and inclusive, in any order.
    pub fn columns(first: u32, last: u32) -> Result<Self, RefError> {
        let a = CellRef::new(0, first)?;
        let b = CellRef::new(MAX_ROWS - 1, last)?;
        Ok(Self::normalized(a, b, RangeShape::Columns))
    }

    /// Whole rows, 0-based and inclusive, in any order.
    pub fn rows(first: u32, last: u32) -> Result<Self, RefError> {
        let a = CellRef::new(first, 0)?;
        let b = CellRef::new(last, MAX_COLS - 1)?;
        Ok(Self::normalized(a, b, RangeShape::Rows))
    }

    fn normalized(a: CellRef, b: CellRef, shape: RangeShape) -> Self {
        let (top, bottom) = if a.row <= b.row { (a, b) } else { (b, a) };
        let (left, right) = if a.col <= b.col { (a, b) } else { (b, a) };
        let start = CellRef {
            row: top.row,
            row_absolute: top.row_absolute,
            col: left.col,
            col_absolute: left.col_absolute,
        };
        let end = CellRef {
            row: bottom.row,
            row_absolute: bottom.row_absolute,
            col: right.col,
            col_absolute: right.col_absolute,
        };
        Range { start, end, shape }
    }

    /// Parses `A1`, `A1:C10`, `$A$1:$C$10`, `A:C` or `2:5` (no sheet name).
    pub fn parse(input: &str) -> Result<Self, RefError> {
        if input.is_empty() {
            return Err(RefError::Empty);
        }
        if input.contains('!') {
            return Err(invalid(
                input,
                "it has a sheet name; parse it as a sheet range instead",
            ));
        }
        let mut pieces = input.split(':');
        let first = pieces.next().unwrap_or_default();
        let second = pieces.next();
        if pieces.next().is_some() {
            return Err(invalid(input, "a range has at most one ':'"));
        }
        let Some(second) = second else {
            return match parse_part(first, input)? {
                Part::Cell(cell) => Ok(Range::cell(cell)),
                Part::Col { .. } => Err(invalid(
                    input,
                    "a whole column is written with ':', like A:A",
                )),
                Part::Row { .. } => {
                    Err(invalid(input, "a whole row is written with ':', like 3:3"))
                }
            };
        };
        match (parse_part(first, input)?, parse_part(second, input)?) {
            (Part::Cell(a), Part::Cell(b)) => Ok(Self::normalized(a, b, RangeShape::Cells)),
            (
                Part::Col {
                    col: c1,
                    absolute: a1,
                },
                Part::Col {
                    col: c2,
                    absolute: a2,
                },
            ) => {
                let a = CellRef {
                    row: 0,
                    col: c1,
                    row_absolute: false,
                    col_absolute: a1,
                };
                let b = CellRef {
                    row: MAX_ROWS - 1,
                    col: c2,
                    row_absolute: false,
                    col_absolute: a2,
                };
                Ok(Self::normalized(a, b, RangeShape::Columns))
            }
            (
                Part::Row {
                    row: r1,
                    absolute: a1,
                },
                Part::Row {
                    row: r2,
                    absolute: a2,
                },
            ) => {
                let a = CellRef {
                    row: r1,
                    col: 0,
                    row_absolute: a1,
                    col_absolute: false,
                };
                let b = CellRef {
                    row: r2,
                    col: MAX_COLS - 1,
                    row_absolute: a2,
                    col_absolute: false,
                };
                Ok(Self::normalized(a, b, RangeShape::Rows))
            }
            _ => Err(invalid(
                input,
                "both sides of ':' must be cells (A1:C3), columns (A:C) or rows (1:3)",
            )),
        }
    }

    /// The same range with `start` at the top-left and `end` at the
    /// bottom-right corner. Only ranges built by hand need it.
    pub fn sorted(&self) -> Range {
        if self.start.row <= self.end.row && self.start.col <= self.end.col {
            *self
        } else {
            Self::normalized(self.start, self.end, self.shape)
        }
    }

    /// Number of rows covered.
    pub fn row_count(&self) -> u32 {
        self.start.row.abs_diff(self.end.row).saturating_add(1)
    }

    /// Number of columns covered.
    pub fn col_count(&self) -> u32 {
        self.start.col.abs_diff(self.end.col).saturating_add(1)
    }

    /// Number of cells covered (up to about 17 billion for a whole sheet).
    pub fn cell_count(&self) -> u64 {
        u64::from(self.row_count()) * u64::from(self.col_count())
    }

    /// True when the range is a single cell.
    pub fn is_single_cell(&self) -> bool {
        self.shape == RangeShape::Cells && self.start.same_cell(&self.end)
    }

    /// True when the 0-based row and column are inside the range.
    pub fn contains(&self, row: u32, col: u32) -> bool {
        let r = self.sorted();
        (r.start.row..=r.end.row).contains(&row) && (r.start.col..=r.end.col).contains(&col)
    }

    /// The cells both ranges cover, as a plain cell range, or `None`.
    pub fn intersect(&self, other: &Range) -> Option<Range> {
        let (a, b) = (self.sorted(), other.sorted());
        let top = a.start.row.max(b.start.row);
        let bottom = a.end.row.min(b.end.row);
        let left = a.start.col.max(b.start.col);
        let right = a.end.col.min(b.end.col);
        if top > bottom || left > right {
            return None;
        }
        Some(Range::new(
            CellRef {
                row: top,
                col: left,
                row_absolute: false,
                col_absolute: false,
            },
            CellRef {
                row: bottom,
                col: right,
                row_absolute: false,
                col_absolute: false,
            },
        ))
    }

    /// True when two ranges share at least one cell.
    pub fn overlaps(&self, other: &Range) -> bool {
        self.intersect(other).is_some()
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dollar = |abs: bool| if abs { "$" } else { "" };
        match self.shape {
            RangeShape::Cells => {
                if self.start == self.end {
                    write!(f, "{}", self.start)
                } else {
                    write!(f, "{}:{}", self.start, self.end)
                }
            }
            RangeShape::Columns => write!(
                f,
                "{}{}:{}{}",
                dollar(self.start.col_absolute),
                column_to_letters(self.start.col),
                dollar(self.end.col_absolute),
                column_to_letters(self.end.col)
            ),
            RangeShape::Rows => write!(
                f,
                "{}{}:{}{}",
                dollar(self.start.row_absolute),
                u64::from(self.start.row) + 1,
                dollar(self.end.row_absolute),
                u64::from(self.end.row) + 1
            ),
        }
    }
}

impl FromStr for Range {
    type Err = RefError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Range::parse(s)
    }
}

/// Checks a sheet name against Excel's rules: 1 to 31 characters (UTF-16),
/// none of `: \ / ? * [ ]`, no `'` at either end, and not `History`.
pub fn validate_sheet_name(name: &str) -> Result<(), RefError> {
    let bad = |reason| {
        Err(RefError::InvalidSheetName {
            name: name.to_owned(),
            reason,
        })
    };
    if name.is_empty() {
        return bad("it is empty");
    }
    if name.encode_utf16().count() > MAX_SHEET_NAME_LEN {
        return bad("it is longer than 31 characters");
    }
    if name.contains(FORBIDDEN_SHEET_CHARS) {
        return bad("it contains one of : \\ / ? * [ ]");
    }
    if name.starts_with('\'') || name.ends_with('\'') {
        return bad("it starts or ends with '");
    }
    if name.chars().any(char::is_control) {
        return bad("it contains a control character");
    }
    if name.eq_ignore_ascii_case("history") {
        return bad("Excel reserves the name History");
    }
    Ok(())
}

/// True when a sheet name must be quoted in a reference (`'My sheet'!A1`).
///
/// Errs on the side of quoting: anything but ASCII letters, digits, `_` and
/// `.` is quoted, as are names that start with a digit or `.`, and names that
/// look like a cell (`A1`), an R1C1 reference (`R1C1`, `R`, `C2`) or a boolean.
/// Excel reads quoted names in every case, so extra quotes are harmless.
pub fn sheet_name_needs_quotes(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return true;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return true;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return true;
    }
    if name.eq_ignore_ascii_case("true") || name.eq_ignore_ascii_case("false") {
        return true;
    }
    if CellRef::parse(name).is_ok() {
        return true;
    }
    looks_like_r1c1(name)
}

/// `R`, `C`, `R1`, `C1`, `R1C1`, `RC`, in any case.
fn looks_like_r1c1(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let rest = upper
        .strip_prefix('R')
        .map(|r| r.trim_start_matches(|c: char| c.is_ascii_digit()));
    let rest = match rest {
        Some(r) => r,
        None => upper.as_str(),
    };
    match rest.strip_prefix('C') {
        Some(r) => r.bytes().all(|b| b.is_ascii_digit()),
        None => rest.is_empty(),
    }
}

/// A sheet name as written in a reference: quoted (with `'` doubled) when needed.
pub fn format_sheet_name(name: &str) -> String {
    if sheet_name_needs_quotes(name) {
        format!("'{}'", name.replace('\'', "''"))
    } else {
        name.to_owned()
    }
}

/// A range with an optional sheet name: `A1:C3`, `Sheet1!A1:C3`, `'Q1 売上'!B2`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SheetRange {
    /// The sheet name, unquoted. `None` means the current sheet.
    pub sheet: Option<String>,
    pub range: Range,
}

impl SheetRange {
    /// A range on a named sheet.
    pub fn new(sheet: impl Into<String>, range: Range) -> Self {
        SheetRange {
            sheet: Some(sheet.into()),
            range,
        }
    }

    /// Parses a range with an optional sheet name. Quoted names may hold any
    /// character Excel allows, with `'` written as `''`. Unquoted names may use
    /// letters (any script), digits, `_` and `.`.
    pub fn parse(input: &str) -> Result<Self, RefError> {
        if input.is_empty() {
            return Err(RefError::Empty);
        }
        if let Some(quoted) = input.strip_prefix('\'') {
            let mut name = String::new();
            let mut chars = quoted.char_indices();
            let mut close = None;
            while let Some((i, c)) = chars.next() {
                if c == '\'' {
                    if quoted[i + 1..].starts_with('\'') {
                        name.push('\'');
                        chars.next();
                    } else {
                        close = Some(i + 1);
                        break;
                    }
                } else {
                    name.push(c);
                }
            }
            let Some(after) = close else {
                return Err(invalid(input, "the quoted sheet name has no closing '"));
            };
            let Some(range_text) = quoted[after..].strip_prefix('!') else {
                return Err(invalid(
                    input,
                    "a quoted sheet name must be followed by '!'",
                ));
            };
            validate_sheet_name(&name)?;
            let range = Range::parse(range_text).map_err(|e| rewrap(e, input))?;
            return Ok(SheetRange {
                sheet: Some(name),
                range,
            });
        }
        match input.split_once('!') {
            None => Ok(SheetRange {
                sheet: None,
                range: Range::parse(input)?,
            }),
            Some((name, range_text)) => {
                if name.is_empty() {
                    return Err(invalid(input, "the sheet name before '!' is empty"));
                }
                if name.contains(':') {
                    return Err(invalid(
                        input,
                        "references across several sheets (Sheet1:Sheet3!A1) are not supported",
                    ));
                }
                if !name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
                {
                    return Err(invalid(
                        input,
                        "put sheet names with spaces or symbols in single quotes, like 'My sheet'!A1",
                    ));
                }
                validate_sheet_name(name)?;
                let range = Range::parse(range_text).map_err(|e| rewrap(e, input))?;
                Ok(SheetRange {
                    sheet: Some(name.to_owned()),
                    range,
                })
            }
        }
    }
}

/// Reports an error on the range part against the whole input.
fn rewrap(err: RefError, input: &str) -> RefError {
    match err {
        RefError::Empty => invalid(input, "the range after '!' is empty"),
        RefError::Invalid { reason, .. } => invalid(input, reason),
        RefError::RowOutOfRange { .. } => RefError::RowOutOfRange {
            input: input.to_owned(),
        },
        RefError::ColumnOutOfRange { .. } => RefError::ColumnOutOfRange {
            input: input.to_owned(),
        },
        other => other,
    }
}

impl fmt::Display for SheetRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(sheet) = &self.sheet {
            write!(f, "{}!", format_sheet_name(sheet))?;
        }
        write!(f, "{}", self.range)
    }
}

impl FromStr for SheetRange {
    type Err = RefError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SheetRange::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(s: &str) -> CellRef {
        CellRef::parse(s).unwrap()
    }

    #[test]
    fn column_letters_round_trip() {
        let cases = [
            (0, "A"),
            (1, "B"),
            (25, "Z"),
            (26, "AA"),
            (27, "AB"),
            (51, "AZ"),
            (52, "BA"),
            (701, "ZZ"),
            (702, "AAA"),
            (16_383, "XFD"),
        ];
        for (n, s) in cases {
            assert_eq!(column_to_letters(n), s, "{n}");
            assert_eq!(column_from_letters(s), Some(n), "{s}");
        }
        for n in 0..MAX_COLS {
            assert_eq!(column_from_letters(&column_to_letters(n)), Some(n));
        }
    }

    #[test]
    fn column_letters_reject() {
        assert_eq!(column_from_letters(""), None);
        assert_eq!(column_from_letters("XFE"), None);
        assert_eq!(column_from_letters("ZZZ"), None);
        assert_eq!(column_from_letters("AAAA"), None);
        assert_eq!(column_from_letters("A1"), None);
        assert_eq!(column_from_letters("é"), None);
        assert_eq!(column_from_letters("xfd"), Some(16_383));
    }

    #[test]
    fn cell_parse_basic() {
        let c = cell("B7");
        assert_eq!((c.row, c.col), (6, 1));
        assert!(!c.row_absolute && !c.col_absolute);
        assert_eq!(c.to_string(), "B7");
    }

    #[test]
    fn cell_parse_absolute_forms() {
        for (s, col_abs, row_abs) in [
            ("$A$1", true, true),
            ("$A1", true, false),
            ("A$1", false, true),
            ("A1", false, false),
        ] {
            let c = cell(s);
            assert_eq!((c.col_absolute, c.row_absolute), (col_abs, row_abs), "{s}");
            assert_eq!(c.to_string(), s);
        }
    }

    #[test]
    fn cell_parse_lower_case_and_leading_zeros() {
        assert_eq!(cell("xfd1").to_string(), "XFD1");
        assert_eq!(cell("a01").to_string(), "A1");
    }

    #[test]
    fn cell_limits() {
        let last = cell("XFD1048576");
        assert_eq!((last.row, last.col), (MAX_ROWS - 1, MAX_COLS - 1));
        assert!(matches!(
            CellRef::parse("A1048577"),
            Err(RefError::RowOutOfRange { .. })
        ));
        assert!(matches!(
            CellRef::parse("XFE1"),
            Err(RefError::ColumnOutOfRange { .. })
        ));
        assert!(matches!(
            CellRef::parse("A0"),
            Err(RefError::RowOutOfRange { .. })
        ));
        assert!(matches!(
            CellRef::parse("A99999999999999999999"),
            Err(RefError::RowOutOfRange { .. })
        ));
        assert!(CellRef::new(MAX_ROWS, 0).is_err());
        assert!(CellRef::new(0, MAX_COLS).is_err());
        assert!(CellRef::new(MAX_ROWS - 1, MAX_COLS - 1).is_ok());
    }

    #[test]
    fn cell_rejects_garbage() {
        for s in [
            "", "A", "1", "$", "$$A1", "A$$1", "1A", "A1B", "A 1", " A1", "A1 ", "A-1", "A1.5",
            "Ａ1", "売1", "$1", "A$",
        ] {
            assert!(CellRef::parse(s).is_err(), "{s:?} should fail");
        }
        assert_eq!(CellRef::parse(""), Err(RefError::Empty));
    }

    #[test]
    fn cell_absolute_relative_helpers() {
        let c = cell("C3");
        assert_eq!(c.absolute().to_string(), "$C$3");
        assert_eq!(c.absolute().relative(), c);
        assert!(c.same_cell(&c.absolute()));
        assert_eq!("C3".parse::<CellRef>().unwrap(), c);
    }

    #[test]
    fn range_single_cell() {
        let r = Range::parse("C3").unwrap();
        assert!(r.is_single_cell());
        assert_eq!(r.cell_count(), 1);
        assert_eq!(r.to_string(), "C3");
    }

    #[test]
    fn range_cells() {
        let r = Range::parse("A1:C10").unwrap();
        assert_eq!(r.shape, RangeShape::Cells);
        assert_eq!((r.row_count(), r.col_count(), r.cell_count()), (10, 3, 30));
        assert_eq!(r.to_string(), "A1:C10");
        assert!(r.contains(0, 0) && r.contains(9, 2));
        assert!(!r.contains(10, 0) && !r.contains(0, 3));
    }

    #[test]
    fn range_same_cell_twice_keeps_colon() {
        assert_eq!(Range::parse("A1:A1").unwrap().to_string(), "A1");
        assert_eq!(Range::parse("$A$1:A1").unwrap().to_string(), "$A$1:A1");
    }

    #[test]
    fn range_normalizes_corners() {
        assert_eq!(Range::parse("C10:A1").unwrap().to_string(), "A1:C10");
        assert_eq!(Range::parse("A10:C1").unwrap().to_string(), "A1:C10");
        assert_eq!(Range::parse("$C$10:A1").unwrap().to_string(), "A1:$C$10");
        assert_eq!(Range::parse("C:A").unwrap().to_string(), "A:C");
        assert_eq!(Range::parse("5:2").unwrap().to_string(), "2:5");
    }

    #[test]
    fn range_absolute() {
        let r = Range::parse("$A$1:$B$2").unwrap();
        assert!(r.start.row_absolute && r.end.col_absolute);
        assert_eq!(r.to_string(), "$A$1:$B$2");
    }

    #[test]
    fn range_whole_columns() {
        let r = Range::parse("A:C").unwrap();
        assert_eq!(r.shape, RangeShape::Columns);
        assert_eq!(r.row_count(), MAX_ROWS);
        assert_eq!(r.col_count(), 3);
        assert_eq!(r.to_string(), "A:C");
        assert_eq!(Range::parse("$a:$c").unwrap().to_string(), "$A:$C");
        assert_eq!(Range::parse("XFD:XFD").unwrap().col_count(), 1);
        assert_eq!(Range::columns(2, 0).unwrap().to_string(), "A:C");
    }

    #[test]
    fn hand_built_ranges_never_panic() {
        let (a1, c3) = (cell("A1"), cell("C3"));
        let reversed = Range {
            start: c3,
            end: a1,
            shape: RangeShape::Cells,
        };
        assert_eq!((reversed.row_count(), reversed.col_count()), (3, 3));
        assert_eq!(reversed.cell_count(), 9);
        assert!(reversed.contains(1, 1) && !reversed.contains(3, 0));
        assert_eq!(reversed.sorted(), Range::new(a1, c3));
        assert_eq!(
            reversed.intersect(&Range::parse("B2:D4").unwrap()),
            Some(Range::parse("B2:C3").unwrap())
        );
        assert!(Range::parse("D4").unwrap().intersect(&reversed).is_none());
        assert_eq!(reversed.to_string(), "C3:A1");

        let huge = CellRef {
            row: u32::MAX,
            col: u32::MAX,
            row_absolute: false,
            col_absolute: false,
        };
        let r = Range {
            start: a1,
            end: huge,
            shape: RangeShape::Cells,
        };
        assert_eq!(r.row_count(), u32::MAX);
        assert_eq!(r.col_count(), u32::MAX);
        assert!(r.contains(u32::MAX, 0));
        assert!(!r.to_string().is_empty());
    }

    #[test]
    fn range_whole_rows() {
        let r = Range::parse("2:5").unwrap();
        assert_eq!(r.shape, RangeShape::Rows);
        assert_eq!((r.row_count(), r.col_count()), (4, MAX_COLS));
        assert_eq!(r.to_string(), "2:5");
        assert_eq!(
            Range::parse("$1:$1048576").unwrap().to_string(),
            "$1:$1048576"
        );
        assert_eq!(Range::rows(4, 1).unwrap().to_string(), "2:5");
        assert!(Range::rows(0, MAX_ROWS).is_err());
    }

    #[test]
    fn whole_sheet_cell_count_does_not_overflow() {
        let r = Range::parse("A1:XFD1048576").unwrap();
        assert_eq!(r.cell_count(), u64::from(MAX_ROWS) * u64::from(MAX_COLS));
        assert_eq!(
            Range::parse("1:1048576").unwrap().cell_count(),
            r.cell_count()
        );
    }

    #[test]
    fn range_rejects() {
        for s in [
            "",
            ":",
            "A1:",
            ":A1",
            "A1:B2:C3",
            "A:1",
            "A1:C",
            "1:A1",
            "A",
            "1",
            "A1:XFE1",
            "A1:A1048577",
            "Sheet1!A1",
            "A1;B2",
            "A1..B2",
            "A0:B2",
        ] {
            assert!(Range::parse(s).is_err(), "{s:?} should fail");
        }
        assert!(matches!(
            Range::parse("A1:XFE1"),
            Err(RefError::ColumnOutOfRange { input }) if input == "A1:XFE1"
        ));
    }

    #[test]
    fn range_intersect() {
        let a = Range::parse("A1:C3").unwrap();
        let b = Range::parse("B2:D4").unwrap();
        assert_eq!(a.intersect(&b).unwrap().to_string(), "B2:C3");
        assert!(a.overlaps(&b));
        let c = Range::parse("E5:F6").unwrap();
        assert!(a.intersect(&c).is_none());
        let col = Range::parse("B:B").unwrap();
        assert_eq!(a.intersect(&col).unwrap().to_string(), "B1:B3");
    }

    #[test]
    fn sheet_names_valid_and_invalid() {
        for ok in [
            "Sheet1",
            "売上",
            "Q1 売上",
            "It's",
            "a.b",
            "1",
            "データ_2026",
        ] {
            assert!(validate_sheet_name(ok).is_ok(), "{ok}");
        }
        let long = "x".repeat(32);
        for bad in [
            "",
            "a:b",
            "a/b",
            "a\\b",
            "a?",
            "a*",
            "[a]",
            "'a",
            "a'",
            "History",
            "history",
            long.as_str(),
            "a\u{7}",
        ] {
            assert!(validate_sheet_name(bad).is_err(), "{bad:?}");
        }
        // 31 CJK characters fit (one UTF-16 unit each); an emoji takes two.
        assert!(validate_sheet_name(&"表".repeat(31)).is_ok());
        assert!(validate_sheet_name(&"😀".repeat(16)).is_err());
    }

    #[test]
    fn sheet_name_quoting() {
        for plain in ["Sheet1", "Data", "_x", "a.b", "Totals2026x"] {
            assert!(!sheet_name_needs_quotes(plain), "{plain}");
            assert_eq!(format_sheet_name(plain), plain);
        }
        for quoted in [
            "My sheet", "売上", "1st", ".a", "A1", "xfd1", "R1C1", "R", "C", "rc", "R2", "C10",
            "TRUE", "false", "a-b", "a!b", "It's",
        ] {
            assert!(sheet_name_needs_quotes(quoted), "{quoted}");
        }
        assert_eq!(format_sheet_name("It's"), "'It''s'");
        assert_eq!(format_sheet_name("Q1 売上"), "'Q1 売上'");
        // Names that only start like a reference stay plain.
        assert!(!sheet_name_needs_quotes("Report"));
        assert!(!sheet_name_needs_quotes("Cost"));
        assert!(!sheet_name_needs_quotes("ABCD1"));
    }

    #[test]
    fn sheet_range_unqualified() {
        let r = SheetRange::parse("A1:B2").unwrap();
        assert_eq!(r.sheet, None);
        assert_eq!(r.to_string(), "A1:B2");
    }

    #[test]
    fn sheet_range_plain_name() {
        let r = SheetRange::parse("Sheet1!$A$1:C3").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("Sheet1"));
        assert_eq!(r.to_string(), "Sheet1!$A$1:C3");
    }

    #[test]
    fn sheet_range_quoted_cjk() {
        let r = SheetRange::parse("'Q1 売上'!B2:D9").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("Q1 売上"));
        assert_eq!(r.range.to_string(), "B2:D9");
        assert_eq!(r.to_string(), "'Q1 売上'!B2:D9");

        let r = SheetRange::parse("'売上'!A1").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("売上"));
        assert_eq!(r.to_string(), "'売上'!A1");
    }

    #[test]
    fn sheet_range_unquoted_cjk_is_accepted_and_quoted_on_output() {
        let r = SheetRange::parse("売上!A1").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("売上"));
        assert_eq!(r.to_string(), "'売上'!A1");
        let r = SheetRange::parse("시트1!A:A").unwrap();
        assert_eq!(r.to_string(), "'시트1'!A:A");
    }

    #[test]
    fn sheet_range_escaped_quote() {
        let r = SheetRange::parse("'It''s'!A1").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("It's"));
        assert_eq!(r.to_string(), "'It''s'!A1");
        let r = SheetRange::parse("'a''''b'!A1").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("a''b"));
        assert_eq!(r.to_string(), "'a''''b'!A1");
    }

    #[test]
    fn sheet_range_quoted_with_bang_and_colon_like_text() {
        let r = SheetRange::parse("'a!b'!C3").unwrap();
        assert_eq!(r.sheet.as_deref(), Some("a!b"));
        assert_eq!(r.to_string(), "'a!b'!C3");
    }

    #[test]
    fn sheet_range_round_trips() {
        for s in [
            "Sheet1!A1",
            "'My sheet'!A1:B2",
            "'Q1 売上'!$A$1:$C$3",
            "Data!A:C",
            "Data!2:5",
            "'1st'!A1",
            "'A1'!B2",
            "'It''s'!XFD1048576",
            "A1",
        ] {
            let r = SheetRange::parse(s).unwrap();
            assert_eq!(r.to_string(), s);
            assert_eq!(SheetRange::parse(&r.to_string()).unwrap(), r);
        }
    }

    #[test]
    fn sheet_range_rejects() {
        for s in [
            "",
            "!A1",
            "''!A1",
            "'Sheet1!A1",
            "'Sheet1'A1",
            "'Sheet1'!",
            "Sheet1!",
            "My sheet!A1",
            "Sheet1:Sheet3!A1",
            "Sheet1!A1:B2:C3",
            "'a:b'!A1",
            "'History'!A1",
            "Sheet1!XFE1",
            "a-b!A1",
        ] {
            assert!(SheetRange::parse(s).is_err(), "{s:?} should fail");
        }
    }

    #[test]
    fn sheet_range_errors_name_the_whole_input() {
        let err = SheetRange::parse("Sheet1!A0").unwrap_err();
        assert_eq!(
            err,
            RefError::RowOutOfRange {
                input: "Sheet1!A0".into()
            }
        );
        let msg = SheetRange::parse("My sheet!A1").unwrap_err().to_string();
        assert!(msg.contains("single quotes"), "{msg}");
        assert!(
            SheetRange::parse("Sheet1:Sheet3!A1")
                .unwrap_err()
                .to_string()
                .contains("several sheets")
        );
    }

    #[test]
    fn error_messages_are_readable() {
        assert_eq!(RefError::Empty.to_string(), "the reference is empty");
        let msg = CellRef::parse("XFE1").unwrap_err().to_string();
        assert!(msg.contains("XFD"), "{msg}");
        let msg = CellRef::parse("A0").unwrap_err().to_string();
        assert!(msg.contains("1048576"), "{msg}");
    }

    #[test]
    fn hostile_long_input_is_rejected_quickly() {
        let long = "A".repeat(100_000);
        assert!(CellRef::parse(&long).is_err());
        assert!(Range::parse(&format!("{long}:{long}")).is_err());
        let quoted = format!("'{}", "x".repeat(100_000));
        assert!(SheetRange::parse(&quoted).is_err());
    }
}
