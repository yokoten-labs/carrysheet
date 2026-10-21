//! A neutral workbook model for Carrysheet.
//!
//! The model is independent of any calculation engine. File readers fill it,
//! engines compute on it (or on their own copy), and writers save it. It keeps
//! what it does not understand in [`Workbook::unknown_parts`] so a save never
//! drops data.
//!
//! Formulas are stored as en-US A1 text without the leading `=`, for example
//! `SUM(A1:A3)`.

mod reference;
mod value;
mod workbook;

pub use reference::{
    CellRef, MAX_COLS, MAX_ROWS, MAX_SHEET_NAME_LEN, Range, RangeShape, RefError, SheetRange,
    column_from_letters, column_to_letters, format_sheet_name, sheet_name_needs_quotes,
    validate_sheet_name,
};
pub use value::{CellValue, ErrorKind};
pub use workbook::{Cell, DateSystem, DefinedName, ModelError, Sheet, UnknownPart, Workbook};
