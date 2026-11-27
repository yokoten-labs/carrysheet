//! A1 ranges as arguments write them: `B7`, `A1:C10`, `A:C`, `2:5`, with an
//! optional sheet name (`Data!A1`, `'Q1 売上'!B2`).
//!
//! GridCraft reads the cells and columns (`gridcraft_core::addr`). This file
//! adds what its `RangeRef::parse` does not do: the sheet name, the `$`
//! marks and the written shape (shown back as written), strict input, and
//! an error that says why.

use std::fmt;

use gridcraft_engine::core::addr::parse_a1_prefix;
use gridcraft_engine::core::{
    CellRef, MAX_COLS, MAX_ROWS, RangeRef, col_to_letters, letters_to_col,
};

/// Why a reference or sheet name could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefError(String);

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn invalid(input: &str, reason: &str) -> RefError {
    RefError(format!("\"{input}\" is not a valid reference: {reason}"))
}

/// Checks a sheet name against Excel's rules: 1 to 31 characters (UTF-16),
/// none of `: \ / ? * [ ]`, no `'` at either end, and not `History`.
pub fn validate_sheet_name(name: &str) -> Result<(), RefError> {
    let reason = if name.is_empty() {
        "it is empty"
    } else if name.encode_utf16().count() > 31 {
        "it is longer than 31 characters"
    } else if name.contains([':', '\\', '/', '?', '*', '[', ']']) {
        "it contains one of : \\ / ? * [ ]"
    } else if name.starts_with('\'') || name.ends_with('\'') {
        "it starts or ends with '"
    } else if name.chars().any(char::is_control) {
        "it contains a control character"
    } else if name.eq_ignore_ascii_case("history") {
        "Excel reserves the name History"
    } else {
        return Ok(());
    };
    Err(RefError(format!(
        "\"{name}\" is not a valid sheet name: {reason}"
    )))
}

/// True when a sheet name must be quoted in a reference (`'My sheet'!A1`).
///
/// Errs on the side of quoting: anything but ASCII letters, digits, `_` and
/// `.` is quoted, as are names that start with a digit or `.`, and names that
/// look like a cell (`A1`), an R1C1 reference (`R1C1`, `R`, `C2`) or a boolean.
pub fn sheet_name_needs_quotes(name: &str) -> bool {
    let plain = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    // `R`, `C`, `R1`, `C1`, `R1C1`, `RC`, in any case.
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let upper = name.to_ascii_uppercase();
    let after_row = upper.strip_prefix('R').map_or(upper.as_str(), |rest| {
        rest.trim_start_matches(|c: char| c.is_ascii_digit())
    });
    let r1c1 = after_row
        .strip_prefix('C')
        .map_or(after_row.is_empty(), digits);
    !plain || r1c1 || CellRef::parse(name).is_some() || upper == "TRUE" || upper == "FALSE"
}

/// A sheet name as written in a reference: quoted (with `'` doubled) when needed.
pub fn format_sheet_name(name: &str) -> String {
    if sheet_name_needs_quotes(name) {
        format!("'{}'", name.replace('\'', "''"))
    } else {
        name.to_owned()
    }
}

/// Cells as text, such as `A1:C3`, or `B7` for one cell.
pub fn cells_text(range: RangeRef) -> String {
    if range.is_single() {
        range.start.a1()
    } else {
        format!("{}:{}", range.start.a1(), range.end.a1())
    }
}

/// How a range was written, so it is shown the same way back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    /// `A1` or `A1:C3`.
    Cells,
    /// Whole columns, `A:C`.
    Columns,
    /// Whole rows, `2:5`.
    Rows,
}

/// A range with an optional sheet name: `A1:C3`, `Sheet1!A:C`, `'Q1 売上'!B2`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SheetRange {
    /// The sheet name, unquoted. `None` means the first sheet.
    pub sheet: Option<String>,
    /// The cells, corners in order; whole columns and rows span the sheet.
    pub range: RangeRef,
    pub shape: Shape,
    /// The `$` marks: start column, start row, end column, end row.
    absolute: [bool; 4],
}

/// One end of a range: its column and its row, each with its `$`. A whole
/// column has no row, and a whole row no column.
type End = (Option<(u32, bool)>, Option<(u32, bool)>);

fn end(part: &str) -> Option<End> {
    if let Some((col, row, col_absolute, row_absolute, "")) = parse_a1_prefix(part) {
        return Some((Some((col, col_absolute)), Some((row, row_absolute))));
    }
    let (absolute, bare) = match part.strip_prefix('$') {
        Some(bare) => (true, bare),
        None => (false, part),
    };
    if let Some(col) = letters_to_col(bare) {
        return Some((Some((col, absolute)), None));
    }
    let is_number = (1..=7).contains(&bare.len()) && bare.bytes().all(|b| b.is_ascii_digit());
    let row: u32 = bare.parse().ok().filter(|_| is_number)?;
    let row = row.checked_sub(1).filter(|row| *row < MAX_ROWS)?;
    Some((None, Some((row, absolute))))
}

/// The cells of `text` (no sheet name), with their shape and `$` marks.
fn cells(text: &str) -> Option<(RangeRef, Shape, [bool; 4])> {
    let (first, second) = match text.split_once(':') {
        Some((a, b)) => (end(a)?, Some(end(b)?)),
        None => (end(text)?, None),
    };
    let last = second.unwrap_or(first);
    let shape = match (first, last) {
        ((Some(_), Some(_)), (Some(_), Some(_))) => Shape::Cells,
        ((Some(_), None), (Some(_), None)) if second.is_some() => Shape::Columns,
        ((None, Some(_)), (None, Some(_))) if second.is_some() => Shape::Rows,
        _ => return None,
    };
    // Corners in order, as Excel reads `B2:A1`; a `$` stays with its line.
    let ordered = |a: Option<(u32, bool)>, b: Option<(u32, bool)>, max: u32| {
        let (a, b) = (a.unwrap_or((0, false)), b.unwrap_or((max, false)));
        if a.0 <= b.0 { (a, b) } else { (b, a) }
    };
    let (left, right) = ordered(first.0, last.0, MAX_COLS - 1);
    let (top, bottom) = ordered(first.1, last.1, MAX_ROWS - 1);
    let range = RangeRef {
        start: CellRef::new(top.0, left.0),
        end: CellRef::new(bottom.0, right.0),
    };
    Some((range, shape, [left.1, top.1, right.1, bottom.1]))
}

/// Why `text` is not a range, said about the whole `input`.
fn why_not(text: &str, input: &str) -> RefError {
    if text.is_empty() {
        return invalid(input, "the range after '!' is empty");
    }
    for part in text.split(':').filter(|part| end(part).is_none()) {
        let bare = part.strip_prefix('$').unwrap_or(part);
        let digits = bare.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        let letters = bare.strip_suffix(digits).unwrap_or(bare);
        let digits = digits.strip_prefix('$').unwrap_or(digits);
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if !letters.is_empty() && letters_to_col(letters).is_none() {
            return RefError(format!(
                "\"{input}\" has a column past XFD, the last of the {MAX_COLS} columns"
            ));
        }
        if !digits.is_empty() {
            return RefError(format!(
                "\"{input}\" has a row outside 1 to {MAX_ROWS}, the rows a sheet can have"
            ));
        }
    }
    invalid(
        input,
        "a range is cells like B7 or A1:C10, whole columns like A:C, or whole rows like 2:5",
    )
}

impl SheetRange {
    /// Parses a range with an optional sheet name. Quoted names may hold any
    /// character Excel allows, with `'` written as `''`. Unquoted names may use
    /// letters (any script), digits, `_` and `.`.
    pub fn parse(input: &str) -> Result<SheetRange, RefError> {
        if input.is_empty() {
            return Err(RefError("the reference is empty".to_owned()));
        }
        let (sheet, text) = if let Some(mut rest) = input.strip_prefix('\'') {
            let mut name = String::new();
            let after = loop {
                let Some((before, after)) = rest.split_once('\'') else {
                    return Err(invalid(input, "the quoted sheet name has no closing '"));
                };
                name.push_str(before);
                match after.strip_prefix('\'') {
                    Some(more) => {
                        name.push('\'');
                        rest = more;
                    }
                    None => break after,
                }
            };
            let Some(text) = after.strip_prefix('!') else {
                let reason = "a quoted sheet name must be followed by '!'";
                return Err(invalid(input, reason));
            };
            (Some(name), text)
        } else {
            match input.split_once('!') {
                None => (None, input),
                Some(("", _)) => {
                    return Err(invalid(input, "the sheet name before '!' is empty"));
                }
                Some((name, _)) if name.contains(':') => {
                    let reason =
                        "references across several sheets (Sheet1:Sheet3!A1) are not supported";
                    return Err(invalid(input, reason));
                }
                Some((name, text)) => {
                    let plain = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
                    if !name.chars().all(plain) {
                        let reason = "put sheet names with spaces or symbols in single quotes, like 'My sheet'!A1";
                        return Err(invalid(input, reason));
                    }
                    (Some(name.to_owned()), text)
                }
            }
        };
        if let Some(name) = &sheet {
            validate_sheet_name(name)?;
        }
        let Some((range, shape, absolute)) = cells(text) else {
            return Err(why_not(text, input));
        };
        Ok(SheetRange {
            sheet,
            range,
            shape,
            absolute,
        })
    }

    /// The range without its sheet name, as written: `$A$1:C3`, `A:C`, `2:5`.
    pub fn a1(&self) -> String {
        let [left, top, right, bottom] = self
            .absolute
            .map(|absolute| if absolute { "$" } else { "" });
        let (start, end) = (self.range.start, self.range.end);
        let (first_col, last_col) = (col_to_letters(start.col), col_to_letters(end.col));
        let (first_row, last_row) = (u64::from(start.row) + 1, u64::from(end.row) + 1);
        match self.shape {
            Shape::Columns => format!("{left}{first_col}:{right}{last_col}"),
            Shape::Rows => format!("{top}{first_row}:{bottom}{last_row}"),
            Shape::Cells if start == end && [left, top] == [right, bottom] => {
                format!("{left}{first_col}{top}{first_row}")
            }
            Shape::Cells => {
                format!("{left}{first_col}{top}{first_row}:{right}{last_col}{bottom}{last_row}")
            }
        }
    }
}

impl fmt::Display for SheetRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(sheet) = &self.sheet {
            write!(f, "{}!", format_sheet_name(sheet))?;
        }
        f.write_str(&self.a1())
    }
}
