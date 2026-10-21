//! The workbook as read from a .xls file.
//!
//! These types are the reader's own. They keep what a later writer needs to
//! round-trip the file: cell values with their XF index, formula tokens next
//! to their text, and every record the reader does not model, in order.

use std::sync::Arc;

pub(crate) mod formats;
pub(crate) mod summary;

/// The BIFF version of the workbook stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BiffVersion {
    /// Excel 2.x.
    Biff2,
    /// Excel 3.0.
    Biff3,
    /// Excel 4.0.
    Biff4,
    /// Excel 5.0 and Excel 95 (also called BIFF7).
    Biff5,
    /// Excel 97 to 2003.
    Biff8,
}

impl BiffVersion {
    /// A short name such as "BIFF8".
    pub fn name(self) -> &'static str {
        match self {
            BiffVersion::Biff2 => "BIFF2",
            BiffVersion::Biff3 => "BIFF3",
            BiffVersion::Biff4 => "BIFF4",
            BiffVersion::Biff5 => "BIFF5",
            BiffVersion::Biff8 => "BIFF8",
        }
    }

    /// The Excel versions that write this format.
    pub fn excel(self) -> &'static str {
        match self {
            BiffVersion::Biff2 => "Excel 2.x",
            BiffVersion::Biff3 => "Excel 3.0",
            BiffVersion::Biff4 => "Excel 4.0",
            BiffVersion::Biff5 => "Excel 5.0/95",
            BiffVersion::Biff8 => "Excel 97-2003",
        }
    }
}

/// Where the workbook stream came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Container {
    /// An OLE2 compound file; `stream` is the name of the workbook stream
    /// ("Workbook" for BIFF8, "Book" for BIFF5, as spelled in the file).
    Ole2 { stream: String },
    /// A bare BIFF stream (Excel 2.x to 4.0 .xls and .xlw files).
    Raw,
}

/// The kind of a sheet substream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetKind {
    Worksheet,
    Chart,
    /// An Excel 4.0 macro sheet.
    MacroSheet,
    /// An Excel 5.0/95 Visual Basic module.
    VbModule,
    /// An Excel 5.0 dialog sheet.
    DialogSheet,
    Other(u16),
}

/// Whether a sheet tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    Hidden,
    /// Hidden so that only a macro can show it again.
    VeryHidden,
}

/// A rectangle of cells, 0-based and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellRange {
    pub first_row: u32,
    pub first_col: u16,
    pub last_row: u32,
    pub last_col: u16,
}

impl CellRange {
    pub fn contains(&self, row: u32, col: u16) -> bool {
        row >= self.first_row
            && row <= self.last_row
            && col >= self.first_col
            && col <= self.last_col
    }

    /// The range in A1 notation, such as "A1:C10" (or "B2" for one cell).
    pub fn to_a1(&self) -> String {
        let a = cell_name(self.first_row, self.first_col);
        if self.first_row == self.last_row && self.first_col == self.last_col {
            a
        } else {
            format!("{a}:{}", cell_name(self.last_row, self.last_col))
        }
    }
}

/// "A", "B", ..., "Z", "AA", ... for a 0-based column.
pub fn column_name(col: u16) -> String {
    let mut n = col as u32 + 1;
    let mut out = Vec::new();
    while n > 0 {
        let r = (n - 1) % 26;
        out.push(b'A' + r as u8);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// "A1" style name of a 0-based cell.
pub fn cell_name(row: u32, col: u16) -> String {
    format!("{}{}", column_name(col), row as u64 + 1)
}

/// Excel's error values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellError {
    Null,
    Div0,
    Value,
    Ref,
    Name,
    Num,
    NA,
    GettingData,
    /// A code Excel does not define.
    Other(u8),
}

impl CellError {
    pub fn from_code(code: u8) -> Self {
        match code {
            0x00 => CellError::Null,
            0x07 => CellError::Div0,
            0x0F => CellError::Value,
            0x17 => CellError::Ref,
            0x1D => CellError::Name,
            0x24 => CellError::Num,
            0x2A => CellError::NA,
            0x2B => CellError::GettingData,
            c => CellError::Other(c),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            CellError::Null => 0x00,
            CellError::Div0 => 0x07,
            CellError::Value => 0x0F,
            CellError::Ref => 0x17,
            CellError::Name => 0x1D,
            CellError::Num => 0x24,
            CellError::NA => 0x2A,
            CellError::GettingData => 0x2B,
            CellError::Other(c) => c,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CellError::Null => "#NULL!",
            CellError::Div0 => "#DIV/0!",
            CellError::Value => "#VALUE!",
            CellError::Ref => "#REF!",
            CellError::Name => "#NAME?",
            CellError::Num => "#NUM!",
            CellError::NA => "#N/A",
            CellError::GettingData => "#GETTING_DATA",
            CellError::Other(_) => "#ERROR!",
        }
    }
}

/// A cell's value. For a formula cell this is the cached result.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    /// A formatted cell without a value (BLANK, MULBLANK), or a formula whose
    /// cached result is an empty string.
    Empty,
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    Error(CellError),
}

/// One formatting run inside a rich string: from character `start` on, use
/// font `font`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatRun {
    pub start: u16,
    pub font: u16,
}

/// The rendered text of a formula, or why it could not be rendered.
#[derive(Debug, Clone, PartialEq)]
pub enum FormulaText {
    /// en-US A1 text without the leading "=", such as `SUM(A1:B2)*2`.
    Parsed(String),
    /// The tokens could not be turned into text. The tokens themselves are
    /// still in [`Formula::tokens`].
    Unparsed {
        reason: String,
        /// Byte offset of the problem inside the tokens.
        at: usize,
    },
}

impl FormulaText {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FormulaText::Parsed(s) => Some(s),
            FormulaText::Unparsed { .. } => None,
        }
    }
}

/// How a cell's formula is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormulaKind {
    /// The cell's own tokens.
    Normal,
    /// Part of a shared formula (SHRFMLA) anchored at the given cell.
    Shared { anchor_row: u32, anchor_col: u16 },
    /// Part of an array formula (ARRAY) over the given range.
    Array { range: CellRange },
    /// Part of a what-if data table (see [`BiffSheet::data_tables`]).
    Table { anchor_row: u32, anchor_col: u16 },
}

/// A parsed formula.
#[derive(Debug, Clone, PartialEq)]
pub struct Formula {
    pub text: FormulaText,
    pub kind: FormulaKind,
    /// The token bytes (rgce) exactly as stored in the record. For a cell
    /// that is part of a shared or array formula, these are the cell's own
    /// tokens (a single PtgExp or PtgTbl).
    pub tokens: Vec<u8>,
    /// Extra data that follows the tokens (rgcb: array constants and
    /// memory-area lists), exactly as stored.
    pub extra: Vec<u8>,
    /// The FORMULA record's option flags (always-calc, shared, ...).
    pub flags: u16,
}

/// A cell record.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub row: u32,
    pub col: u16,
    /// XF (cell format) index. For BIFF2 it is taken from the cell
    /// attributes (or the IXFE record).
    pub xf: u16,
    /// Index into [`BiffWorkbook::number_formats`].
    pub format: u32,
    pub value: CellValue,
    pub formula: Option<Box<Formula>>,
    /// Formatting runs of a rich-text value (RSTRING, or an SST entry).
    pub runs: Option<Arc<[FormatRun]>>,
}

/// A COLINFO (or BIFF2 COLWIDTH) record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnInfo {
    pub first_col: u16,
    pub last_col: u16,
    /// Width in 1/256 of the width of the zero character.
    pub width: u16,
    pub xf: u16,
    pub hidden: bool,
    pub outline_level: u8,
    pub collapsed: bool,
    /// The raw option flags.
    pub flags: u16,
}

/// A shared formula definition (SHRFMLA).
#[derive(Debug, Clone, PartialEq)]
pub struct SharedFormula {
    pub range: CellRange,
    /// The reserved byte after the range, as stored.
    pub reserved: u8,
    /// How many cells use the formula (cUse), as stored.
    pub uses: u8,
    pub tokens: Vec<u8>,
    pub extra: Vec<u8>,
}

/// An array formula definition (ARRAY).
#[derive(Debug, Clone, PartialEq)]
pub struct ArrayFormula {
    pub range: CellRange,
    pub flags: u16,
    pub tokens: Vec<u8>,
    pub extra: Vec<u8>,
    pub text: FormulaText,
}

/// A what-if data table (TABLE record). The record is also kept raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataTable {
    /// The result cells of the table.
    pub range: CellRange,
    /// The formula Excel shows in each result cell, such as `TABLE(B1,)`.
    pub text: String,
}

/// A record the reader keeps as bytes so a writer can put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRecord {
    pub record_type: u16,
    /// Position among all records of its substream (BOF is 0).
    pub index: u32,
    /// Stream offset of the record header.
    pub offset: u64,
    pub data: Vec<u8>,
}

/// A sheet substream.
#[derive(Debug, Clone, PartialEq)]
pub struct BiffSheet {
    pub name: String,
    pub kind: SheetKind,
    pub visibility: Visibility,
    /// Cells sorted by row, then column. Later records win over earlier ones
    /// for the same cell.
    pub cells: Vec<Cell>,
    /// The DIMENSIONS record, if present (last row and column inclusive).
    pub dimensions: Option<CellRange>,
    pub merged: Vec<CellRange>,
    pub columns: Vec<ColumnInfo>,
    pub shared_formulas: Vec<SharedFormula>,
    pub array_formulas: Vec<ArrayFormula>,
    pub data_tables: Vec<DataTable>,
    /// Records the reader does not model, in stream order. Records inside
    /// an embedded chart substream are kept here too.
    pub raw_records: Vec<RawRecord>,
    /// Stream offset of the sheet's BOF record.
    pub offset: u64,
}

impl BiffSheet {
    /// The cell at a position, if there is a record for it.
    pub fn cell(&self, row: u32, col: u16) -> Option<&Cell> {
        self.cells
            .binary_search_by(|c| (c.row, c.col).cmp(&(row, col)))
            .ok()
            .map(|i| &self.cells[i])
    }
}

/// A defined name (NAME record).
#[derive(Debug, Clone, PartialEq)]
pub struct DefinedName {
    /// The name. Built-in names use Excel's English spelling, such as
    /// "Print_Area".
    pub name: String,
    /// The built-in name code, if this is one (0x06 is Print_Area, ...).
    pub builtin: Option<u8>,
    /// 0-based sheet index for a sheet-local name, `None` for a workbook name.
    pub sheet: Option<u16>,
    pub hidden: bool,
    /// The name stands for a macro function or command.
    pub function: bool,
    /// Keyboard shortcut character (macro names).
    pub key: u8,
    /// The raw option flags.
    pub flags: u16,
    /// BIFF5: the EXTERNSHEET index (ixals) the name belongs to; BIFF8: the
    /// reserved field there. 0 before BIFF5.
    pub extern_index: u16,
    /// Lengths of the custom menu, description, help topic and status bar
    /// texts (BIFF5 and BIFF8), which are at the end of `formula.extra`.
    pub text_lens: [u8; 4],
    /// The name's formula. Its `extra` holds every byte after the tokens:
    /// array constants first, then the menu, description, help and status
    /// texts, as stored.
    pub formula: Formula,
}

/// An SST entry.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedString {
    pub text: Arc<str>,
    pub runs: Option<Arc<[FormatRun]>>,
    /// Phonetic data (ExtRst), kept as stored.
    pub phonetic: Vec<u8>,
}

/// A problem that did not stop the read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Stream offset of the record involved.
    pub offset: u64,
    pub message: String,
}

/// A whole workbook.
#[derive(Debug, Clone, PartialEq)]
pub struct BiffWorkbook {
    pub version: BiffVersion,
    pub container: Container,
    /// True for an Excel 4.0 workbook (.xlw) with several sheets in one
    /// raw stream.
    pub biff4_workbook: bool,
    /// The CODEPAGE value, if the file has one.
    pub codepage: Option<u16>,
    /// Dates count from 1904-01-01 (DATEMODE record) instead of 1900-01-01.
    pub date_1904: bool,
    pub sheets: Vec<BiffSheet>,
    pub names: Vec<DefinedName>,
    /// Distinct number format strings; [`Cell::format`] indexes this list.
    /// Entry 0 is always "General".
    pub number_formats: Vec<String>,
    /// The workbook's FORMAT records: (format id, format string). In BIFF2
    /// to BIFF4 the id is the record's position.
    pub formats: Vec<(u16, String)>,
    /// Format id of each XF record, in order.
    pub xf_formats: Vec<u16>,
    pub shared_strings: Vec<SharedString>,
    /// Workbook-globals records the reader does not model, in stream order.
    pub raw_records: Vec<RawRecord>,
    pub warnings: Vec<Warning>,
}

impl BiffWorkbook {
    /// The number format string of a cell.
    pub fn number_format(&self, cell: &Cell) -> &str {
        self.number_formats
            .get(cell.format as usize)
            .map(String::as_str)
            .unwrap_or("General")
    }

    /// True if the cell holds a number shown as a date or time.
    pub fn is_date(&self, cell: &Cell) -> bool {
        matches!(cell.value, CellValue::Number(_))
            && formats::is_date_format(self.number_format(cell))
    }

    /// The sheet with this name (case-insensitive, as Excel compares).
    pub fn sheet(&self, name: &str) -> Option<&BiffSheet> {
        self.sheets
            .iter()
            .find(|s| s.name.to_lowercase() == name.to_lowercase())
    }

    /// A summary of every sheet, for an overview listing.
    pub fn summaries(&self) -> Vec<summary::SheetSummary> {
        self.sheets
            .iter()
            .map(|s| summary::summarize(self, s))
            .collect()
    }
}
