//! Looking at a workbook: `overview`, `read` in pages, and `eval`.
//!
//! Each command takes arguments checked by
//! [`validate_args`](crate::commands::validate_args) (absent ones are `null`)
//! and returns the JSON fields of the result.

use std::fmt;

use gridcraft_engine::core::{CellRef, DateSystem, RangeRef, Value as CellValue, col_to_letters};
use gridcraft_engine::model::{Sheet, Workbook};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::args::{detailed, input, number, shown, text};
use crate::commands::limits::{MAX_CELL_TEXT_CHARS, MAX_LISTED_SHEETS, MAX_RESPONSE_TEXT_CHARS};
use crate::error::{CommandError, ErrorCode};
use crate::open::{Book, SourceKind};
use crate::range::{Shape, SheetRange, cells_text, format_sheet_name};
use crate::save::clean_from_file;
use crate::{Kernel, internal};

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
    /// What GridCraft skipped or repaired while reading the file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// When the file holds macros (which kinds, never run, kept on save or
    /// not), or that they were not checked. Absent: none, or a csv file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macros: Option<String>,
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
    fn band_height(&self, area: &RangeRef) -> u32 {
        let (max_rows, max_columns, max_cells) = self.limits();
        let width = max_columns.min(area.width());
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
    area: RangeRef,
    next: CellRef,
    band_end_row: u32,
}

impl ReadCursor {
    /// The first page of `area` on `sheet`.
    pub fn start(sheet: &str, area: RangeRef, request: &ReadRequest) -> ReadCursor {
        let band_end_row = area.end.row.min(
            area.start
                .row
                .saturating_add(request.band_height(&area) - 1),
        );
        ReadCursor {
            sheet: sheet.to_owned(),
            area,
            next: area.start,
            band_end_row,
        }
    }

    /// The sheet being read.
    pub fn sheet(&self) -> &str {
        &self.sheet
    }

    /// The cells the next page may cover: its band of rows, from its first
    /// column to the area's last.
    pub fn rest_of_band(&self) -> RangeRef {
        RangeRef::new(
            self.next,
            CellRef::new(self.band_end_row, self.area.end.col),
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
        let area = area.range;
        let band = SheetRange::parse(band_text)
            .ok()
            .filter(|band| band.sheet.is_none())
            .ok_or_else(|| bad("the part after '@' is not a range"))?
            .range;
        let cursor = ReadCursor {
            sheet,
            area,
            next: band.start,
            band_end_row: band.end.row,
        };
        let inside = area.contains(cursor.next)
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
    pub fn plan(
        &self,
        request: &ReadRequest,
    ) -> Result<(RangeRef, Option<ReadCursor>), CommandError> {
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
        let page = RangeRef::new(self.next, CellRef::new(self.band_end_row, last_col));
        let next = if last_col < self.area.end.col {
            Some(ReadCursor {
                next: CellRef::new(self.next.row, last_col + 1),
                ..self.clone()
            })
        } else if self.band_end_row < self.area.end.row {
            let top = self.band_end_row + 1;
            Some(ReadCursor {
                next: CellRef::new(top, self.area.start.col),
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
        let band = RangeRef::new(self.next, CellRef::new(self.band_end_row, self.next.col));
        let sheet = format_sheet_name(&self.sheet);
        write!(f, "{sheet}!{}@{}", cells_text(self.area), cells_text(band))
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
            MAX_CELL_TEXT_CHARS,
            self.max_chars + 1,
            MAX_CELL_TEXT_CHARS,
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
/// stays under [`MAX_RESPONSE_TEXT_CHARS`].
pub fn text_room(cells: u64) -> usize {
    let each = MAX_RESPONSE_TEXT_CHARS as u64 / cells.max(1);
    usize::try_from(each)
        .unwrap_or(usize::MAX)
        .clamp(16, MAX_CELL_TEXT_CHARS)
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
/// their text (`#DIV/0!`), empty as `null`. An array shows its top-left value.
pub fn value_json(v: &CellValue) -> Value {
    match v {
        CellValue::Empty => Value::Null,
        CellValue::Number(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(n.to_string())),
        CellValue::Text(s) => Value::String(s.to_string()),
        CellValue::Bool(b) => Value::Bool(*b),
        CellValue::Error(e) => Value::String(e.as_str().to_owned()),
        CellValue::Array(a) => a.data.first().map_or(Value::Null, value_json),
    }
}

/// The type name used in output: `empty`, `number`, `text`, `bool`, `error`.
fn kind(v: &CellValue) -> &'static str {
    match v {
        CellValue::Empty => "empty",
        CellValue::Number(_) => "number",
        CellValue::Text(_) => "text",
        CellValue::Bool(_) => "bool",
        CellValue::Error(_) => "error",
        CellValue::Array(a) => a.data.first().map_or("empty", kind),
    }
}

/// `v` with text cut at `max` characters (counted in `cut`), without
/// copying the whole text first. An array gives its top-left value.
fn cut_value(v: &CellValue, max: usize, cut: &mut u64) -> CellValue {
    match v {
        CellValue::Text(t) => match cut_text(t, max) {
            Some(short) => {
                *cut += 1;
                CellValue::Text(short.into())
            }
            None => v.clone(),
        },
        CellValue::Array(a) => a
            .data
            .first()
            .map(|v| cut_value(v, max, cut))
            .unwrap_or_default(),
        v => v.clone(),
    }
}

/// The part of `wanted` worth reading, given the sheet's used range, or
/// `None` when it holds no data. The end is clipped to the used range, so
/// whole columns don't produce a million rows. Whole columns also lose the
/// empty rows above the data, and whole rows the empty columns left of it;
/// a cell range keeps the start it asked for.
pub fn clip_to_used(wanted: &SheetRange, used: RangeRef) -> Option<RangeRef> {
    let (start, end) = (wanted.range.start, wanted.range.end);
    let top = match wanted.shape {
        Shape::Columns => start.row.max(used.start.row),
        Shape::Rows | Shape::Cells => start.row,
    };
    let left = match wanted.shape {
        Shape::Rows => start.col.max(used.start.col),
        Shape::Columns | Shape::Cells => start.col,
    };
    let bottom = end.row.min(used.end.row);
    let right = end.col.min(used.end.col);
    (top <= bottom && left <= right)
        .then(|| RangeRef::new(CellRef::new(top, left), CellRef::new(bottom, right)))
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

/// One sheet, as reads see it.
struct View<'a> {
    wb: &'a Workbook,
    sh: &'a Sheet,
}

impl View<'_> {
    /// The smallest range holding every cell with content; `None` when empty.
    fn used_range(&self) -> Option<RangeRef> {
        // Cells with only a style do not count as used.
        let mut bounds: Option<RangeRef> = None;
        let mut grow = |at: CellRef| {
            let one = RangeRef::cell(at);
            bounds = Some(bounds.map_or(one, |all| all.union(&one)));
        };
        for (at, cell) in self.sh.cells.iter() {
            if cell.formula.is_some() || !cell.value.is_empty() {
                grow(at);
            }
        }
        for r in self.sh.spill_ranges.values() {
            grow(r.start);
            grow(r.end);
        }
        // `None` past the grid.
        bounds.filter(|all| all.end.is_valid())
    }

    /// The cell at a 0-based row and column, with its text and formula cut
    /// at `max_chars` characters, and whether anything was cut; `None` when
    /// empty.
    fn cell_cut(&self, at: CellRef, max_chars: usize) -> Option<(CellInfo, bool)> {
        let mut cut = 0u64;
        let formula = self.sh.cell(at).and_then(|c| c.formula.as_ref()).map(|f| {
            let f = f.text.trim_start_matches('=');
            cut_text(f, max_chars)
                .inspect(|_| cut += 1)
                .unwrap_or_else(|| f.to_owned())
        });
        let value = cut_value(&self.sh.value(at), max_chars, &mut cut);
        if value.is_empty() && formula.is_none() {
            return None;
        }
        let format = self
            .wb
            .styles
            .get(self.sh.style_id(at))
            .num_fmt
            .as_str()
            .to_owned();
        Some((
            CellInfo {
                value,
                formula,
                format: (!format.eq_ignore_ascii_case("General")).then_some(format),
            },
            cut > 0,
        ))
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
        obj.insert("type".into(), json!(kind(&cell.value)));
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
fn read_page(sheet: &View<'_>, request: &ReadRequest) -> Result<ReadPage, CommandError> {
    let name = &sheet.sh.name;
    let empty_page = || ReadPage {
        sheet: name.clone(),
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
            let area = match &request.range {
                Some(wanted) => clip_to_used(wanted, used),
                None => Some(used),
            };
            let Some(area) = area else {
                return Ok(empty_page());
            };
            ReadCursor::start(name, area, request)
        }
    };
    let (page, next_cursor) = cursor.plan(request)?;
    let room = text_room(page.count());
    let mut cut = CutText::new(room);
    let sheet_name = format_sheet_name(name);
    let rows = (page.start.row..=page.end.row)
        .map(|r| ReadRow {
            row: r + 1,
            cells: (page.start.col..=page.end.col)
                .map(|c| {
                    let at = CellRef::new(r, c);
                    let cell = sheet.cell_cut(at, room).map(|(info, was_cut)| {
                        if was_cut {
                            cut.add(format!("{sheet_name}!{at}"));
                        }
                        info
                    });
                    cell_json(cell, request)
                })
                .collect(),
        })
        .collect();
    Ok(ReadPage {
        sheet: name.clone(),
        range: Some(cells_text(page)),
        columns: (page.start.col..=page.end.col)
            .map(col_to_letters)
            .collect(),
        rows,
        next_cursor,
        cut: cut.finish(),
    })
}

/// Largest array a formula may return to `eval`.
const MAX_EVAL_CELLS: usize = 10_000;

impl Book {
    pub(crate) fn overview(&self) -> Result<Overview, CommandError> {
        let wb = self.wb()?;
        let sheets = wb
            .sheets
            .iter()
            .map(|sh| {
                let (mut cells, mut formulas, mut errors) = (0, 0, 0);
                for (_, cell) in sh.cells.iter() {
                    if cell.formula.is_some() {
                        formulas += 1;
                    }
                    if cell.formula.is_some() || !cell.value.is_empty() {
                        cells += 1;
                    }
                    if matches!(cell.value, CellValue::Error(_)) {
                        errors += 1;
                    }
                }
                SheetOverview {
                    name: sh.name.clone(),
                    used_range: View { wb, sh }.used_range().map(cells_text),
                    cells,
                    formulas,
                    errors,
                }
            })
            .collect();
        Ok(Overview {
            sheets,
            date_system: match wb.date_system {
                DateSystem::D1904 => "1904",
                _ => "1900",
            }
            .to_owned(),
            defined_names: wb.names.len(),
            warnings: self
                .warnings
                .iter()
                .map(|w| clean_from_file(w, 200))
                .collect(),
            macros: match self.kind {
                SourceKind::Package => crate::save::loss::macros(&self.source, true),
                // An .xlsb is a zip with the same part names.
                SourceKind::Imported("xlsb") => crate::save::loss::macros(&self.source, false),
                SourceKind::Imported(format) => Some(format!(
                    "not checked: Carrysheet cannot see macros in .{format} files; it never runs them"
                )),
                SourceKind::Csv => None,
            },
        })
    }

    pub(crate) fn read(&self, request: &ReadRequest) -> Result<ReadPage, CommandError> {
        let wb = self.wb()?;
        let si = self.sheet_index(requested_sheet(request))?;
        let sh = wb
            .sheets
            .get(si)
            .ok_or_else(|| internal("reading cells", "sheet index out of range"))?;
        read_page(&View { wb, sh }, request)
    }

    pub(crate) fn evaluate(
        &self,
        sheet: Option<&str>,
        formula: &str,
    ) -> Result<Evaluation, CommandError> {
        let wb = self.wb()?;
        let si = self.sheet_index(sheet)?;
        let body = formula.trim();
        let body = body.strip_prefix('=').unwrap_or(body).trim();
        let expr = gridcraft_engine::formula::parse(body).map_err(|e| {
            CommandError::new(
                ErrorCode::Formula,
                "the formula could not be read",
                e.to_string(),
                "Write an en-US formula with commas between arguments, like SUM(A1:A3) or IF(A1>0,\"yes\",\"no\").",
            )
        })?;
        let value =
            gridcraft_engine::calc::recalc::evaluate_expr(wb, si, CellRef::new(0, 0), &expr);
        let mut cut = 0u64;
        let (value, kind, room) = match &value {
            CellValue::Array(a) => {
                if a.data.len() > MAX_EVAL_CELLS {
                    return Err(CommandError::new(
                        ErrorCode::LimitExceeded,
                        format!("the result has {} cells", a.data.len()),
                        format!("eval returns at most {MAX_EVAL_CELLS} cells"),
                        "Wrap the formula in one that sums it up, like SUM(...), ROWS(...) or INDEX(...,1,1).",
                    ));
                }
                let room = text_room(a.data.len() as u64);
                let rows: Vec<Value> = a
                    .data
                    .chunks(a.cols.max(1))
                    .map(|row| {
                        row.iter()
                            .map(|v| value_json(&cut_value(v, room, &mut cut)))
                            .collect()
                    })
                    .collect();
                (json!(rows), "array".to_owned(), room)
            }
            v => {
                let room = text_room(1);
                let v = cut_value(v, room, &mut cut);
                (value_json(&v), kind(&v).to_owned(), room)
            }
        };
        Ok(Evaluation {
            formula: body.to_owned(),
            value,
            kind,
            cut: (cut > 0).then(|| {
                format!(
                    "{cut} text value(s) hold more than {room} characters and were cut. Get the rest with MID, like MID(<text>,{},{}).",
                    room + 1,
                    MAX_CELL_TEXT_CHARS
                )
            }),
        })
    }
}

/// Header cells shown per sheet in `overview`.
const HEADER_CELLS: u32 = 20;
const HEADER_CELLS_DETAILED: u32 = DEFAULT_PAGE_COLUMNS;

/// Sheets, used ranges, header rows and counts.
pub(crate) fn overview(
    kernel: &Kernel,
    args: &Map<String, Value>,
    bytes: Vec<u8>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let book = kernel.open_bytes(&path, bytes)?;
    let overview = kernel.overview(&book)?;
    let width = if detailed(args) {
        HEADER_CELLS_DETAILED
    } else {
        HEADER_CELLS
    };
    let mut sheets = Vec::new();
    // Header rows stop when the answer holds this much text.
    let mut text_left = MAX_RESPONSE_TEXT_CHARS;
    let mut headers_cut = 0usize;
    for s in overview.sheets.iter().take(MAX_LISTED_SHEETS) {
        let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
        v["name"] = json!(clean_from_file(&s.name, 64));
        // The first row of the used range: usually the headers.
        let header = match &s.used_range {
            Some(_) if text_left == 0 => {
                headers_cut += 1;
                Value::Null
            }
            Some(used) => {
                let first_row = SheetRange::parse(used)
                    .ok()
                    .map(|r| r.range.start.row + 1)
                    .unwrap_or(1);
                let range = SheetRange::parse(&format!(
                    "{}!{first_row}:{first_row}",
                    format_sheet_name(&s.name)
                ))?;
                let request = ReadRequest {
                    range: Some(range),
                    max_rows: 1,
                    max_columns: width,
                    ..ReadRequest::default()
                };
                let page = kernel.read_range(&book, &request)?;
                let mut header = json!({
                    "row": first_row,
                    "columns": page.columns,
                    "cells": page.rows.first().map(|r| r.cells.clone()).unwrap_or_default(),
                });
                if let Some(cut) = &page.cut {
                    header["cut"] = json!(cut);
                }
                text_left = text_left.saturating_sub(header.to_string().len());
                header
            }
            None => Value::Null,
        };
        v["header"] = header;
        sheets.push(v);
    }
    let mut data = Map::new();
    data.insert("file".into(), shown(&path));
    data.insert("sheets".into(), Value::Array(sheets));
    data.insert("date_system".into(), json!(overview.date_system));
    data.insert("defined_names".into(), json!(overview.defined_names));
    if let Some(macros) = &overview.macros {
        data.insert("macros".into(), json!(macros));
    }
    if !overview.warnings.is_empty() {
        data.insert("warnings".into(), json!(overview.warnings));
    }
    let total = overview.sheets.len();
    if total > MAX_LISTED_SHEETS || headers_cut > 0 {
        data.insert("sheets_total".into(), json!(total));
        data.insert(
            "hint".into(),
            json!(format!(
                "The answer was cut to stay small: {} of {total} sheet(s) listed, {headers_cut} header row(s) left out. Read a sheet's first row with read and a range like 'Name'!1:1.",
                total.min(MAX_LISTED_SHEETS)
            )),
        );
    }
    Ok(data)
}

/// One page of a range.
pub(crate) fn read(
    kernel: &Kernel,
    args: &Map<String, Value>,
    bytes: Vec<u8>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let mut request = ReadRequest {
        range: text(args, "range").map(SheetRange::parse).transpose()?,
        cursor: text(args, "cursor").map(ReadCursor::parse).transpose()?,
        detailed: detailed(args),
        ..ReadRequest::default()
    };
    if let Some(mode) = text(args, "mode") {
        request.mode = ReadMode::parse(mode).ok_or_else(|| {
            CommandError::invalid_argument(
                "mode",
                "mode is not values, formulas or both",
                "Use values, formulas or both.",
            )
        })?;
    }
    request.max_rows = number(args, "max_rows").unwrap_or(DEFAULT_PAGE_ROWS);
    request.max_columns = number(args, "max_columns").unwrap_or(DEFAULT_PAGE_COLUMNS);
    let book = kernel.open_bytes(&path, bytes)?;
    let page = kernel.read_range(&book, &request)?;
    let more = page.next_cursor.is_some();
    let mut data = match serde_json::to_value(&page) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    data.insert("file".into(), shown(&path));
    if more {
        data.insert(
            "hint".into(),
            json!("More cells remain: call read again with cursor set to next_cursor and the same max_rows, or ask for a smaller range."),
        );
    }
    Ok(data)
}

/// Computes a formula without changing the file.
pub(crate) fn eval(
    kernel: &Kernel,
    args: &Map<String, Value>,
    bytes: Vec<u8>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let formula = text(args, "formula").unwrap_or("");
    if formula.trim().trim_start_matches('=').trim().is_empty() {
        return Err(CommandError::invalid_argument(
            "formula",
            "the formula is empty",
            "Give a formula like SUM(Sheet1!B2:B9).",
        ));
    }
    let book = kernel.open_bytes(&path, bytes)?;
    let result = kernel.evaluate(&book, text(args, "sheet"), formula)?;
    let mut data = match serde_json::to_value(&result) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    data.insert("file".into(), shown(&path));
    Ok(data)
}
