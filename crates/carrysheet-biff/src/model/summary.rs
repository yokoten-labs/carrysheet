//! Per-sheet counts for an overview listing.

use crate::model::{
    BiffSheet, BiffWorkbook, CellRange, CellValue, FormulaText, SheetKind, Visibility,
};

/// What a sheet holds, in numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetSummary {
    pub name: String,
    pub kind: SheetKind,
    pub visibility: Visibility,
    /// Smallest range holding every cell record, formatted blanks included.
    pub used_range: Option<CellRange>,
    /// Smallest range holding every cell with a value or formula.
    pub value_range: Option<CellRange>,
    /// All cell records.
    pub cells: usize,
    pub numbers: usize,
    /// Numbers shown as dates or times.
    pub dates: usize,
    pub texts: usize,
    pub booleans: usize,
    pub errors: usize,
    /// Formatted cells without a value.
    pub blanks: usize,
    pub formulas: usize,
    /// Formulas whose tokens could not be turned into text.
    pub unparsed_formulas: usize,
    pub merged_ranges: usize,
    /// Records kept as bytes.
    pub raw_records: usize,
}

fn grow(range: &mut Option<CellRange>, row: u32, col: u16) {
    match range {
        None => {
            *range = Some(CellRange {
                first_row: row,
                first_col: col,
                last_row: row,
                last_col: col,
            })
        }
        Some(r) => {
            r.first_row = r.first_row.min(row);
            r.first_col = r.first_col.min(col);
            r.last_row = r.last_row.max(row);
            r.last_col = r.last_col.max(col);
        }
    }
}

pub(crate) fn summarize(book: &BiffWorkbook, sheet: &BiffSheet) -> SheetSummary {
    let mut s = SheetSummary {
        name: sheet.name.clone(),
        kind: sheet.kind,
        visibility: sheet.visibility,
        used_range: None,
        value_range: None,
        cells: sheet.cells.len(),
        numbers: 0,
        dates: 0,
        texts: 0,
        booleans: 0,
        errors: 0,
        blanks: 0,
        formulas: 0,
        unparsed_formulas: 0,
        merged_ranges: sheet.merged.len(),
        raw_records: sheet.raw_records.len(),
    };
    for c in &sheet.cells {
        grow(&mut s.used_range, c.row, c.col);
        let has_value = !matches!(c.value, CellValue::Empty) || c.formula.is_some();
        if has_value {
            grow(&mut s.value_range, c.row, c.col);
        }
        match &c.value {
            CellValue::Empty => {
                if c.formula.is_none() {
                    s.blanks += 1
                }
            }
            CellValue::Number(_) => {
                s.numbers += 1;
                if book.is_date(c) {
                    s.dates += 1;
                }
            }
            CellValue::Text(_) => s.texts += 1,
            CellValue::Bool(_) => s.booleans += 1,
            CellValue::Error(_) => s.errors += 1,
        }
        if let Some(f) = &c.formula {
            s.formulas += 1;
            if matches!(f.text, FormulaText::Unparsed { .. }) {
                s.unparsed_formulas += 1;
            }
        }
    }
    s
}
