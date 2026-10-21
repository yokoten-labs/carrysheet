//! The engine interface: what the commands need from a calculation engine.
//!
//! The real engine comes after the kernel comparison picks one. Until then,
//! `MemoryEngine` keeps workbooks in memory for tests; it is built only for
//! this crate's tests and with the `test-engine` feature.

use std::fmt;
use std::path::Path;

use carrysheet_model::{
    CellRef, CellValue, Range, RangeShape, SheetRange, Workbook, column_to_letters,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{CommandError, ErrorCode};

pub mod edit;
pub mod run;
pub mod save;

pub use edit::{CellChange, EditKind, EditOp, EditResult, EditTarget, OpResult, Scalar, parse_ops};
pub use save::{Encoded, Loss, PartCounts, SaveReport};

#[cfg(any(test, feature = "test-engine"))]
mod memory;
#[cfg(any(test, feature = "test-engine"))]
pub use memory::MemoryEngine;

/// File formats Carrysheet writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    Xlsx,
    Xls,
    Csv,
}

impl FileFormat {
    /// The format named by an extension (any case), such as `xlsx`. The
    /// macro-enabled `xlsm` is the same package format as `xlsx`.
    pub fn from_extension(ext: &str) -> Option<FileFormat> {
        match ext.to_ascii_lowercase().as_str() {
            "xlsx" | "xlsm" => Some(FileFormat::Xlsx),
            "xls" => Some(FileFormat::Xls),
            "csv" => Some(FileFormat::Csv),
            _ => None,
        }
    }

    /// The format as written in arguments and results: `xlsx`, `xls`, `csv`.
    pub fn as_str(self) -> &'static str {
        match self {
            FileFormat::Xlsx => "xlsx",
            FileFormat::Xls => "xls",
            FileFormat::Csv => "csv",
        }
    }

    /// The format of a path, from its extension.
    pub fn from_path(path: &Path) -> Option<FileFormat> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(FileFormat::from_extension)
    }
}

/// One sheet in an [`Overview`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SheetOverview {
    pub name: String,
    /// Like `A1:F20`, or `null` for an empty sheet.
    pub used_range: Option<String>,
    pub cells: usize,
    pub formulas: usize,
    pub errors: usize,
}

/// What `overview` returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Overview {
    pub sheets: Vec<SheetOverview>,
    /// `1900` or `1904`.
    pub date_system: String,
    pub defined_names: usize,
    /// What the engine skipped or repaired while reading the file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Overview {
    /// Counts sheets, cells, formulas and errors of a model workbook.
    /// Engines that keep a [`Workbook`] can use this directly.
    pub fn of(wb: &Workbook) -> Overview {
        Overview {
            sheets: wb
                .sheets()
                .iter()
                .map(|s| SheetOverview {
                    name: s.name().to_owned(),
                    used_range: s.used_range().map(|r| r.to_string()),
                    cells: s.cell_count(),
                    formulas: s.cells().filter(|(_, _, c)| c.formula.is_some()).count(),
                    errors: s
                        .cells()
                        .filter(|(_, _, c)| matches!(c.value, CellValue::Error(_)))
                        .count(),
                })
                .collect(),
            date_system: wb.date_system.as_str().to_owned(),
            defined_names: wb.defined_names.len(),
            warnings: Vec::new(),
        }
    }
}

/// What to show for each cell in `read`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadMode {
    #[default]
    Values,
    Formulas,
    Both,
}

impl ReadMode {
    pub fn parse(s: &str) -> Option<ReadMode> {
        match s {
            "values" => Some(ReadMode::Values),
            "formulas" => Some(ReadMode::Formulas),
            "both" => Some(ReadMode::Both),
            _ => None,
        }
    }
}

/// Rows per page when none is asked for.
pub const DEFAULT_PAGE_ROWS: u32 = 200;

/// Columns per page when none is asked for.
pub const DEFAULT_PAGE_COLUMNS: u32 = 50;

/// Cells per page when none is asked for (about 8k tokens of short values).
pub const DEFAULT_PAGE_CELLS: u32 = 10_000;

/// A request for one page of a range.
///
/// A page holds at most `max_rows` rows, `max_columns` columns and
/// `max_cells` cells (each at least 1). A wide range is read in bands of
/// rows: across the band a page at a time, then down to the next band.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadRequest {
    /// The range; `None` means the used range of the first sheet. A range
    /// without a sheet name means the first sheet.
    pub range: Option<SheetRange>,
    /// Where an earlier page said to continue. When set, `range` is ignored.
    pub cursor: Option<ReadCursor>,
    pub mode: ReadMode,
    /// Rows per page.
    pub max_rows: u32,
    /// Columns per page.
    pub max_columns: u32,
    /// Cells per page.
    pub max_cells: u32,
    /// `response_format: detailed`: each cell as an object with its value,
    /// type, formula and number format, whatever `mode` says.
    pub detailed: bool,
}

impl Default for ReadRequest {
    fn default() -> Self {
        ReadRequest {
            range: None,
            cursor: None,
            mode: ReadMode::Values,
            max_rows: DEFAULT_PAGE_ROWS,
            max_columns: DEFAULT_PAGE_COLUMNS,
            max_cells: DEFAULT_PAGE_CELLS,
            detailed: false,
        }
    }
}

impl ReadRequest {
    fn limits(&self) -> (u32, u32, u32) {
        (
            self.max_rows.max(1),
            self.max_columns.max(1),
            self.max_cells.max(1),
        )
    }

    /// How many rows a new band of `area` gets, starting at its first column.
    fn band_height(&self, area: &Range) -> u32 {
        let (max_rows, max_columns, max_cells) = self.limits();
        let width = max_columns.min(area.col_count());
        max_rows.min((max_cells / width).max(1))
    }
}

/// Where a paged read continues: the whole area being read and the next
/// page's place in it. Pages go across a band of rows, then down.
///
/// Written as the area, `@`, then the next page's first column over the
/// rows of its band, such as `Data!A1:CZ500@AY1:AY200`. Agents pass it back
/// as it came; [`ReadCursor::parse`] checks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadCursor {
    sheet: String,
    area: Range,
    next: CellRef,
    band_end_row: u32,
}

impl ReadCursor {
    /// The first page of `area` on `sheet`.
    pub fn start(sheet: &str, area: Range, request: &ReadRequest) -> ReadCursor {
        let area = area.sorted();
        let band_end_row = area.end.row.min(
            area.start
                .row
                .saturating_add(request.band_height(&area) - 1),
        );
        ReadCursor {
            sheet: sheet.to_owned(),
            area,
            next: area.start.relative(),
            band_end_row,
        }
    }

    /// The sheet being read.
    pub fn sheet(&self) -> &str {
        &self.sheet
    }

    /// The whole area being read.
    pub fn area(&self) -> Range {
        self.area
    }

    /// The cells the next page may cover: its band of rows, from its first
    /// column to the area's last.
    pub fn rest_of_band(&self) -> Range {
        Range::new(
            self.next,
            CellRef {
                row: self.band_end_row,
                ..self.area.end.relative()
            },
        )
    }

    /// Reads a cursor written by [`ReadCursor`]'s `Display`.
    pub fn parse(text: &str) -> Result<ReadCursor, CommandError> {
        let bad = |why: &str| {
            CommandError::invalid_argument(
                "cursor",
                format!("the cursor is not one a read returned: {why}"),
                "Pass next_cursor exactly as the last page returned it, or leave it out to start from the top.",
            )
        };
        let (area_text, band_text) = text.rsplit_once('@').ok_or_else(|| bad("it has no '@'"))?;
        let area =
            SheetRange::parse(area_text).map_err(|_| bad("the part before '@' is not a range"))?;
        let sheet = area
            .sheet
            .ok_or_else(|| bad("the range has no sheet name"))?;
        let area = area.range.sorted();
        let band = Range::parse(band_text)
            .map_err(|_| bad("the part after '@' is not a range"))?
            .sorted();
        let cursor = ReadCursor {
            sheet,
            area,
            next: band.start.relative(),
            band_end_row: band.end.row,
        };
        let inside = area.contains(cursor.next.row, cursor.next.col)
            && band.start.col == band.end.col
            && band.end.row <= area.end.row;
        if !inside {
            return Err(bad("the next page lies outside the area"));
        }
        Ok(cursor)
    }

    /// Plans the page this cursor points at, and the cursor after it.
    ///
    /// The cursor fixes the page's rows; `request` sets its width. A cursor
    /// whose band is taller than `request` allows is refused, so a page
    /// never holds more than `max_rows` rows or `max_cells` cells.
    pub fn plan(&self, request: &ReadRequest) -> Result<(Range, Option<ReadCursor>), CommandError> {
        let (max_rows, max_columns, max_cells) = request.limits();
        let height = self.band_end_row - self.next.row + 1;
        if height > max_rows.min(max_cells) {
            return Err(CommandError::invalid_argument(
                "cursor",
                format!(
                    "the cursor continues pages {height} rows tall, more than this request allows"
                ),
                "Read the next page with the same max_rows as the page that gave the cursor, or start again without a cursor.",
            ));
        }
        let width = max_columns
            .min(self.area.end.col - self.next.col + 1)
            .min((max_cells / height).max(1));
        let last_col = self.next.col + width - 1;
        let page = Range::new(
            self.next,
            CellRef {
                row: self.band_end_row,
                col: last_col,
                ..self.next
            },
        );
        let next = if last_col < self.area.end.col {
            Some(ReadCursor {
                next: CellRef {
                    col: last_col + 1,
                    ..self.next
                },
                ..self.clone()
            })
        } else if self.band_end_row < self.area.end.row {
            let top = self.band_end_row + 1;
            Some(ReadCursor {
                next: CellRef {
                    row: top,
                    ..self.area.start.relative()
                },
                band_end_row: self
                    .area
                    .end
                    .row
                    .min(top.saturating_add(request.band_height(&self.area) - 1)),
                ..self.clone()
            })
        } else {
            None
        };
        Ok((page, next))
    }
}

impl fmt::Display for ReadCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let band = Range::new(
            self.next,
            CellRef {
                row: self.band_end_row,
                ..self.next
            },
        );
        write!(
            f,
            "{}@{band}",
            SheetRange::new(self.sheet.as_str(), self.area)
        )
    }
}

impl Serialize for ReadCursor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// One row of a [`ReadPage`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadRow {
    /// 1-based row number, as shown in Excel.
    pub row: u32,
    /// One entry per column in [`ReadPage::columns`].
    pub cells: Vec<Value>,
}

/// One page of a range: a compact table with column letters and row numbers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadPage {
    pub sheet: String,
    /// The part of the range this page covers, like `A1:D200`; `null` when empty.
    pub range: Option<String>,
    /// Column letters, like `["A", "B", "C"]`.
    pub columns: Vec<String>,
    pub rows: Vec<ReadRow>,
    /// Where the next page starts, to pass back as the next request's
    /// cursor; `null` on the last page.
    pub next_cursor: Option<ReadCursor>,
    /// Cells whose text or formula was cut to keep the answer small.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<CutText>,
}

/// Most cells named in [`CutText::cells`].
pub const MAX_LISTED_CUT: usize = 20;

/// Text cut from an answer, and how to get the rest.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CutText {
    /// Each text and formula was cut at this many characters.
    pub max_chars: usize,
    /// The cells cut, like `Sheet1!A1`, at most [`MAX_LISTED_CUT`].
    pub cells: Vec<String>,
    /// All cells cut.
    pub total: u64,
    pub hint: String,
}

impl CutText {
    fn new(max_chars: usize) -> CutText {
        CutText {
            max_chars,
            cells: Vec::new(),
            total: 0,
            hint: String::new(),
        }
    }

    fn add(&mut self, cell: String) {
        self.total += 1;
        if self.cells.len() < MAX_LISTED_CUT {
            self.cells.push(cell);
        }
    }

    /// `None` when nothing was cut; else with the hint filled in.
    fn finish(mut self) -> Option<CutText> {
        let first = self.cells.first()?.clone();
        self.hint = format!(
            "{} cell(s) hold more than {} characters and were cut. Read fewer cells at a time to see more of each (up to {}), or get the rest of one with eval, like MID({first},{},{}).",
            self.total,
            self.max_chars,
            crate::limits::MAX_CELL_TEXT_CHARS,
            self.max_chars + 1,
            crate::limits::MAX_CELL_TEXT_CHARS,
        );
        Some(self)
    }
}

/// `text` cut at `max` characters, or `None` when it is not longer. Counts
/// characters without looking past `max + 1` of them.
pub fn cut_text(text: &str, max: usize) -> Option<String> {
    if text.len() <= max {
        return None;
    }
    let (end, _) = text.char_indices().nth(max)?;
    Some(text.get(..end).unwrap_or(text).to_owned())
}

/// Characters each text in a page of `cells` cells may keep, so the page
/// stays under [`crate::limits::MAX_RESPONSE_TEXT_CHARS`].
pub fn text_room(cells: u64) -> usize {
    let each = crate::limits::MAX_RESPONSE_TEXT_CHARS as u64 / cells.max(1);
    usize::try_from(each)
        .unwrap_or(usize::MAX)
        .clamp(16, crate::limits::MAX_CELL_TEXT_CHARS)
}

/// What `eval` returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evaluation {
    /// The formula as computed, without `=`.
    pub formula: String,
    pub value: Value,
    /// `empty`, `number`, `text`, `bool` or `error`.
    pub kind: String,
    /// Says that long text in the value was cut, and how to get the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<String>,
}

/// A cell value as JSON: numbers and booleans as themselves, errors as
/// their text (`#DIV/0!`), empty as `null`.
pub fn value_json(v: &CellValue) -> Value {
    match v {
        CellValue::Empty => Value::Null,
        CellValue::Number(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(n.to_string())),
        CellValue::Text(s) => Value::String(s.clone()),
        CellValue::Bool(b) => Value::Bool(*b),
        CellValue::Error(e) => Value::String(e.as_str().to_owned()),
    }
}

/// What the commands need from a calculation engine.
///
/// `Book` is the engine's own loaded workbook, so engines need not convert
/// to [`Workbook`] for every call. All methods report failures as
/// [`CommandError`]s with what, why and fix.
pub trait Engine {
    type Book;

    /// A short name for messages and `--version`, such as `memory`.
    fn name(&self) -> &'static str;

    /// Opens a workbook file.
    fn open(&self, path: &Path) -> Result<Self::Book, CommandError>;

    /// Sheets, used ranges and counts.
    fn overview(&self, book: &Self::Book) -> Result<Overview, CommandError>;

    /// One page of a range.
    fn read_range(
        &self,
        book: &Self::Book,
        request: &ReadRequest,
    ) -> Result<ReadPage, CommandError>;

    /// Computes a formula (en-US, with or without `=`) without changing the book.
    /// `sheet` is where unqualified references point; `None` means the first sheet.
    fn evaluate(
        &self,
        book: &Self::Book,
        sheet: Option<&str>,
        formula: &str,
    ) -> Result<Evaluation, CommandError>;

    /// Writes the book to `path` in `format`.
    fn save(&self, book: &Self::Book, path: &Path, format: FileFormat) -> Result<(), CommandError>;

    /// Applies operations in order, all or nothing: on an error the book
    /// is left as it was, or must not be used again.
    fn edit(&self, _book: &mut Self::Book, _ops: &[EditOp]) -> Result<EditResult, CommandError> {
        Err(CommandError::new(
            ErrorCode::NotImplemented,
            format!("the {} engine cannot edit", self.name()),
            "this engine only reads",
            "Use the real engine.",
        ))
    }

    /// Encodes the book for `path` in `format`, and says what the saved
    /// file would lose compared with the file that was opened. Nothing is
    /// written. For csv, `sheet` picks the sheet (default: the first).
    fn encode(
        &self,
        _book: &Self::Book,
        _path: &Path,
        _format: FileFormat,
        _sheet: Option<&str>,
    ) -> Result<Encoded, CommandError> {
        Err(CommandError::new(
            ErrorCode::NotImplemented,
            format!("the {} engine cannot encode files", self.name()),
            "this engine keeps workbooks in memory only",
            "Use the real engine.",
        ))
    }

    /// [`Engine::encode`] with all the options. Engines that write csv
    /// override it to honour `csv_raw`.
    fn encode_with(
        &self,
        book: &Self::Book,
        path: &Path,
        format: FileFormat,
        options: &EncodeOptions<'_>,
    ) -> Result<Encoded, CommandError> {
        self.encode(book, path, format, options.sheet)
    }

    /// Starts one command's budget: the work and time limits cover every
    /// call that follows (open, work, save) until [`Engine::end_call`].
    /// `seconds` replaces the engine's time limit (the CLI's
    /// `--max-seconds`); `None` keeps it.
    fn begin_call(&self, _seconds: Option<f64>) {}

    /// Ends the budget [`Engine::begin_call`] started: an error when a
    /// limit ran out during the command, whatever the calls returned.
    fn end_call(&self) -> Result<(), CommandError> {
        Ok(())
    }
}

/// How [`Engine::encode_with`] writes a file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodeOptions<'a> {
    /// For csv, the sheet to write; `None` means the first.
    pub sheet: Option<&'a str>,
    /// For csv, write text starting with `=`, `+`, `-`, `@`, tab or return
    /// as it is, instead of with a `'` in front (formula injection).
    pub csv_raw: bool,
}

fn no_such_sheet(name: &str) -> CommandError {
    CommandError::new(
        ErrorCode::NoSuchSheet,
        format!("there is no sheet named \"{name}\""),
        "the workbook has no sheet with that name (names are compared without case)",
        "Run overview to see the sheet names.",
    )
}

fn no_sheets() -> CommandError {
    CommandError::new(
        ErrorCode::NoSuchSheet,
        "the workbook has no sheets",
        "there is nothing to read",
        "Add a sheet first.",
    )
}

/// Picks the sheet a reference points at: its own sheet name, else `default`, else the first.
pub(crate) fn pick_sheet<'a>(
    wb: &'a Workbook,
    named: Option<&str>,
    default: Option<&str>,
) -> Result<&'a carrysheet_model::Sheet, CommandError> {
    match named.or(default) {
        Some(name) => wb.sheet(name).ok_or_else(|| no_such_sheet(name)),
        None => wb.sheets().first().ok_or_else(no_sheets),
    }
}

/// The part of `wanted` worth reading, given the sheet's used range, or
/// `None` when it holds no data. The end is clipped to the used range, so
/// whole columns don't produce a million rows. Whole columns also lose the
/// empty rows above the data, and whole rows the empty columns left of it;
/// a cell range keeps the start it asked for.
pub fn clip_to_used(wanted: Range, used: Range) -> Option<Range> {
    let (wanted, used) = (wanted.sorted(), used.sorted());
    let top = match wanted.shape {
        RangeShape::Columns => wanted.start.row.max(used.start.row),
        RangeShape::Rows | RangeShape::Cells => wanted.start.row,
    };
    let left = match wanted.shape {
        RangeShape::Rows => wanted.start.col.max(used.start.col),
        RangeShape::Columns | RangeShape::Cells => wanted.start.col,
    };
    let bottom = wanted.end.row.min(used.end.row);
    let right = wanted.end.col.min(used.end.col);
    if top > bottom || left > right {
        return None;
    }
    Some(Range::new(
        CellRef::new(top, left).ok()?,
        CellRef::new(bottom, right).ok()?,
    ))
}

/// One cell as a read sees it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CellInfo {
    /// The value; for a formula, its computed result.
    pub value: CellValue,
    /// The formula without `=`, such as `SUM(A1:A3)`.
    pub formula: Option<String>,
    /// The number format code, such as `0.00%`; `None` for General.
    pub format: Option<String>,
}

/// A sheet a read can page through. Engines implement it on their own
/// sheet type, so [`read_page`] works for every engine.
pub trait SheetView {
    fn name(&self) -> &str;
    /// The smallest range holding every cell with content; `None` when empty.
    fn used_range(&self) -> Option<Range>;
    /// The cell at a 0-based row and column; `None` when empty.
    fn cell(&self, row: u32, col: u32) -> Option<CellInfo>;

    /// [`SheetView::cell`] with its text and formula cut at `max_chars`
    /// characters, and whether anything was cut. Engines whose cells can
    /// hold huge texts cut them before copying.
    fn cell_cut(&self, row: u32, col: u32, max_chars: usize) -> Option<(CellInfo, bool)> {
        let mut info = self.cell(row, col)?;
        let mut cut = false;
        if let CellValue::Text(t) = &info.value
            && let Some(short) = cut_text(t, max_chars)
        {
            info.value = CellValue::Text(short);
            cut = true;
        }
        if let Some(short) = info.formula.as_deref().and_then(|f| cut_text(f, max_chars)) {
            info.formula = Some(short);
            cut = true;
        }
        Some((info, cut))
    }
}

impl SheetView for carrysheet_model::Sheet {
    fn name(&self) -> &str {
        carrysheet_model::Sheet::name(self)
    }

    fn used_range(&self) -> Option<Range> {
        carrysheet_model::Sheet::used_range(self)
    }

    fn cell(&self, row: u32, col: u32) -> Option<CellInfo> {
        self.get(row, col).map(|c| CellInfo {
            value: c.value.clone(),
            formula: c.formula.clone(),
            format: None,
        })
    }
}

/// How one cell is shown in a page.
fn cell_json(cell: Option<CellInfo>, request: &ReadRequest) -> Value {
    let Some(cell) = cell else {
        return Value::Null;
    };
    if request.detailed {
        let mut obj = serde_json::Map::new();
        obj.insert("value".into(), value_json(&cell.value));
        obj.insert("type".into(), json!(cell.value.kind()));
        if let Some(f) = &cell.formula {
            obj.insert("formula".into(), json!(format!("={f}")));
        }
        if let Some(fmt) = &cell.format {
            obj.insert("format".into(), json!(fmt));
        }
        return Value::Object(obj);
    }
    match (request.mode, &cell.formula) {
        (ReadMode::Values, _) | (ReadMode::Formulas, None) => value_json(&cell.value),
        (ReadMode::Formulas, Some(f)) => Value::String(format!("={f}")),
        (ReadMode::Both, f) => json!({
            "value": value_json(&cell.value),
            "formula": f.as_ref().map(|f| format!("={f}")),
        }),
    }
}

/// The sheet a request reads: the cursor's, else the range's, else `None`
/// for the first sheet.
pub fn requested_sheet(request: &ReadRequest) -> Option<&str> {
    match &request.cursor {
        Some(cursor) => Some(cursor.sheet()),
        None => request.range.as_ref().and_then(|r| r.sheet.as_deref()),
    }
}

/// One page of a range of one sheet: the sheet [`requested_sheet`] names.
pub fn read_page(sheet: &dyn SheetView, request: &ReadRequest) -> Result<ReadPage, CommandError> {
    let empty_page = || ReadPage {
        sheet: sheet.name().to_owned(),
        range: None,
        columns: Vec::new(),
        rows: Vec::new(),
        next_cursor: None,
        cut: None,
    };
    let cursor = match &request.cursor {
        Some(cursor) => cursor.clone(),
        None => {
            let Some(used) = sheet.used_range() else {
                return Ok(empty_page());
            };
            let wanted = request.range.as_ref().map_or(used, |r| r.range);
            let Some(area) = clip_to_used(wanted, used) else {
                return Ok(empty_page());
            };
            ReadCursor::start(sheet.name(), area, request)
        }
    };
    let (page, next_cursor) = cursor.plan(request)?;
    let room = text_room(page.cell_count());
    let mut cut = CutText::new(room);
    let sheet_name = carrysheet_model::format_sheet_name(sheet.name());
    let rows = (page.start.row..=page.end.row)
        .map(|r| ReadRow {
            row: r + 1,
            cells: (page.start.col..=page.end.col)
                .map(|c| {
                    let cell = sheet.cell_cut(r, c, room).map(|(info, was_cut)| {
                        if was_cut {
                            cut.add(format!("{sheet_name}!{}{}", column_to_letters(c), r + 1));
                        }
                        info
                    });
                    cell_json(cell, request)
                })
                .collect(),
        })
        .collect();
    Ok(ReadPage {
        sheet: sheet.name().to_owned(),
        range: Some(page.to_string()),
        columns: (page.start.col..=page.end.col)
            .map(column_to_letters)
            .collect(),
        rows,
        next_cursor,
        cut: cut.finish(),
    })
}

/// One page of a range of a model [`Workbook`]. Engines that keep a
/// [`Workbook`] can use this directly for [`Engine::read_range`].
pub fn read_workbook(book: &Workbook, request: &ReadRequest) -> Result<ReadPage, CommandError> {
    let sheet = pick_sheet(book, requested_sheet(request), None)?;
    read_page(sheet, request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_formats_from_paths() {
        assert_eq!(
            FileFormat::from_path(Path::new("a.XLSX")),
            Some(FileFormat::Xlsx)
        );
        assert_eq!(
            FileFormat::from_path(Path::new("a.xls")),
            Some(FileFormat::Xls)
        );
        assert_eq!(
            FileFormat::from_path(Path::new("a.csv")),
            Some(FileFormat::Csv)
        );
        assert_eq!(FileFormat::from_path(Path::new("a.ods")), None);
        assert_eq!(FileFormat::from_path(Path::new("a")), None);
    }

    #[test]
    fn non_finite_numbers_become_text() {
        assert_eq!(value_json(&CellValue::Number(f64::NAN)), json!("NaN"));
        assert_eq!(value_json(&CellValue::Empty), Value::Null);
    }
}
