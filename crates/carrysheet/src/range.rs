//! A1 ranges as arguments write them: `B7`, `A1:C10`, `A:C`, `2:5`, with an
//! optional sheet name (`Data!A1`, `'Q1 売上'!B2`).
//!
//! GridCraft's `RangeRef::parse` reads the cells. This file adds the sheet
//! name and refuses what GridCraft would let pass (spaces, `+`).

use std::fmt;

use gridcraft_engine::core::RangeRef;
pub use gridcraft_engine::formula::quote_sheet as format_sheet_name;

use crate::text::quote;

/// Why a reference or sheet name could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefError(String);

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn invalid(input: &str, reason: &str) -> RefError {
    RefError(format!("{} is not a valid reference: {reason}", quote(input, 200)))
}

/// A range with an optional sheet name: `A1:C3`, `Sheet1!A:C`, `'Q1 売上'!B2`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SheetRange {
    /// The sheet name, unquoted. `None` means the first sheet.
    pub sheet: Option<String>,
    /// The cells, corners in order; whole columns and rows span the sheet.
    pub range: RangeRef,
}

impl SheetRange {
    /// Parses a range with an optional sheet name. Quoted names may hold any
    /// character Excel allows, with `'` written as `''`. Unquoted names may use
    /// letters (any script), digits, `_` and `.`.
    pub fn parse(input: &str) -> Result<SheetRange, RefError> {
        if input.is_empty() {
            return Err(RefError("the reference is empty".to_owned()));
        }
        // A range never holds `'`, so the last `'!` ends a quoted name.
        let (sheet, text) = if let Some(quoted) = input.strip_prefix('\'') {
            let Some((name, text)) = quoted.rsplit_once("'!") else {
                let reason = "a quoted sheet name ends with '!, like 'My sheet'!A1";
                return Err(invalid(input, reason));
            };
            if name.replace("''", "").contains('\'') {
                return Err(invalid(input, "write a ' inside a sheet name as ''"));
            }
            (Some(name.replace("''", "'")), text)
        } else {
            match input.split_once('!') {
                None => (None, input),
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
        let is_range_char = |c: char| c.is_ascii_alphanumeric() || c == '$' || c == ':';
        let is_strict = text.chars().all(is_range_char) && !text.contains("$$");
        let range = RangeRef::parse(text).filter(|_| is_strict);
        let Some(range) = range else {
            let reason = "write cells like B7 or A1:C10, whole columns like A:C, or whole rows like 2:5; columns go from A to XFD, rows from 1 to 1048576";
            return Err(invalid(input, reason));
        };
        Ok(SheetRange { sheet, range })
    }

    /// The range without its sheet name: `A1:C3`, `A:C`, `2:5`.
    pub fn a1(&self) -> String {
        self.range.a1()
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
