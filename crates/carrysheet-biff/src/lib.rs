//! Reads legacy Excel workbooks: `.xls` files from Excel 2.x to Excel 2003
//! (BIFF2, BIFF3, BIFF4, BIFF5 and BIFF8), and Excel 4.0 `.xlw` workbooks.
//!
//! ```no_run
//! let bytes = std::fs::read("book.xls").unwrap();
//! let book = carrysheet_biff::read(&bytes).unwrap();
//! for summary in book.summaries() {
//!     println!("{}: {} cells", summary.name, summary.cells);
//! }
//! ```
//!
//! The reader treats every file as hostile: it never panics, allocates in
//! proportion to the input, and stops at the [`Limits`]. Records it does not
//! model are kept as bytes ([`RawRecord`]) so that a writer can put them back.
//!
//! Sources: Microsoft's [MS-XLS] specification for BIFF8, and Apache POI
//! (Apache-2.0) and calamine (MIT) for the older versions.

mod codepage;
mod error;
mod formula;
mod model;
mod parse;
mod stream;

pub use error::{BiffError, BiffErrorKind, Encryption};
pub use model::formats::{builtin_format, is_date_format};
pub use model::summary::SheetSummary;
pub use model::{
    ArrayFormula, BiffSheet, BiffVersion, BiffWorkbook, Cell, CellError, CellRange, CellValue,
    ColumnInfo, Container, DataTable, DefinedName, FormatRun, Formula, FormulaKind, FormulaText,
    RawRecord, SharedFormula, SharedString, SheetKind, Visibility, Warning, cell_name, column_name,
};

/// Safety limits. The defaults fit any workbook Excel itself can write.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Size of the workbook stream in bytes.
    pub max_stream_bytes: usize,
    /// Physical records in the stream.
    pub max_records: usize,
    /// CONTINUE records after one record.
    pub max_continues: usize,
    /// Sheets in the workbook.
    pub max_sheets: usize,
    /// Cell records in the whole workbook.
    pub max_cells: usize,
    /// Highest 0-based column a cell may use. BIFF8 allows 255; the default
    /// leaves room for writers that go further.
    pub max_col: u16,
    /// SST entries.
    pub max_shared_strings: usize,
    /// NAME records.
    pub max_names: usize,
    /// Length in bytes of one rendered formula.
    pub max_formula_text: usize,
    /// Length in bytes of all rendered formula text together.
    pub max_total_formula_text: usize,
    /// Formula token bytes rendered, over all formulas, as a multiple of the
    /// stream size (plus 16 MiB). A shared formula counts once for every
    /// cell that uses it.
    pub max_formula_work_per_byte: usize,
    /// Range checks spent matching cells to their shared, array and table
    /// formulas. Past it, cells not yet matched are left unparsed (with a
    /// warning) rather than failing the read.
    pub max_formula_lookups: usize,
    /// Warnings kept; later ones are counted in the last warning.
    pub max_warnings: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_stream_bytes: 512 << 20,
            max_records: 32 << 20,
            max_continues: 65_536,
            max_sheets: 65_536,
            max_cells: 16 << 20,
            max_col: 16_383,
            max_shared_strings: 16 << 20,
            max_names: 65_536,
            max_formula_text: 64 << 10,
            max_total_formula_text: 1 << 30,
            max_formula_work_per_byte: 64,
            max_formula_lookups: 16 << 20,
            max_warnings: 1_000,
        }
    }
}

/// Reads a workbook from the bytes of a `.xls` or `.xlw` file.
pub fn read(bytes: &[u8]) -> Result<BiffWorkbook, BiffError> {
    read_with_limits(bytes, &Limits::default())
}

/// Like [`read`], with your own limits.
pub fn read_with_limits(bytes: &[u8], limits: &Limits) -> Result<BiffWorkbook, BiffError> {
    let (stream, container) = stream::container::workbook_stream(bytes, limits.max_stream_bytes)?;
    parse::parse(&stream, container, limits)
}

/// The raw BIFF workbook stream of a file: the `Workbook` or `Book` stream
/// of an OLE2 file, or the file itself for a raw BIFF2 to BIFF4 file.
/// Useful for tools that list records.
pub fn workbook_stream(bytes: &[u8]) -> Result<Vec<u8>, BiffError> {
    let limits = Limits::default();
    stream::container::workbook_stream(bytes, limits.max_stream_bytes).map(|(s, _)| s.into_owned())
}
