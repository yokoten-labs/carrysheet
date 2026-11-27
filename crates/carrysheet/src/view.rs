//! Looking at a workbook: `overview`, `read` in pages, and `eval`. Each command takes
//! arguments checked by [`validate_args`](crate::commands::validate_args).

use std::fmt;

use gridcraft_engine::core::{CellRef, DateSystem, RangeRef, Value as CellValue, col_to_letters};
use gridcraft_engine::model::{Sheet, Workbook};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::args::{detailed, input, number, shown, text};
use crate::commands::limits::{MAX_CELL_TEXT_CHARS, MAX_LISTED_SHEETS, MAX_RESPONSE_TEXT_CHARS};
use crate::error::{CommandError, ErrorCode};
use crate::open::{Book, SourceKind};
use crate::range::{Shape, SheetRange, format_sheet_name};
use crate::text::escape;
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
    /// Which macros the file holds (never run; kept on save or not), or that they were not checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macros: Option<String>,
    /// What GridCraft skipped or repaired while reading the file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What to show for each cell in `read`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    #[default]
    Values,
    Formulas,
    Both,
}

/// Rows, columns and cells per page when none is asked for (10,000 short values: about 8k tokens).
pub const DEFAULT_PAGE_ROWS: u32 = 200;
pub const DEFAULT_PAGE_COLUMNS: u32 = 50;
pub const DEFAULT_PAGE_CELLS: u32 = 10_000;

/// A request for one page: at most `max_rows` rows, `max_columns` columns and `max_cells` cells
/// (each at least 1). A wide range is read in bands of rows: across a band, then down.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadRequest {
    /// The range; `None` means the used range. No sheet name means the first sheet.
    pub range: Option<SheetRange>,
    /// Where an earlier page said to continue. When set, `range` is ignored.
    pub cursor: Option<ReadCursor>,
    pub mode: ReadMode,
    pub max_rows: u32,
    pub max_columns: u32,
    pub max_cells: u32,
    /// `response_format: detailed`: each cell as an object with value, type, formula and format.
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
        (self.max_rows.max(1), self.max_columns.max(1), self.max_cells.max(1))
    }

    /// The last row of a new band of `area` that starts at row `top`.
    fn band_end(&self, area: &RangeRef, top: u32) -> u32 {
        let (max_rows, max_columns, max_cells) = self.limits();
        let width = max_columns.min(area.width());
        let height = max_rows.min((max_cells / width).max(1));
        area.end.row.min(top.saturating_add(height - 1))
    }
}

/// Where a paged read continues, written as the area, `@`, then the next page's first column
/// over the rows of its band: `Data!A1:CZ500@AY1:AY200`. [`ReadCursor::parse`] checks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadCursor {
    pub(crate) sheet: String,
    area: RangeRef,
    next: CellRef,
    band_end_row: u32,
}

impl ReadCursor {
    /// The first page of `area` on `sheet`.
    pub fn start(sheet: &str, area: RangeRef, request: &ReadRequest) -> ReadCursor {
        ReadCursor {
            sheet: sheet.to_owned(),
            area,
            next: area.start,
            band_end_row: request.band_end(&area, area.start.row),
        }
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
        let area = SheetRange::parse(area_text);
        let area = area.map_err(|_| bad("the part before '@' is not a range"))?;
        let sheet = area.sheet.ok_or_else(|| bad("the range has no sheet name"))?;
        let band = SheetRange::parse(band_text).ok().filter(|band| band.sheet.is_none());
        let band = band.ok_or_else(|| bad("the part after '@' is not a range"))?.range;
        let area = area.range;
        let inside = area.contains(band.start)
            && band.start.col == band.end.col
            && band.end.row <= area.end.row;
        if !inside {
            return Err(bad("the next page lies outside the area"));
        }
        Ok(ReadCursor { sheet, area, next: band.start, band_end_row: band.end.row })
    }

    /// Plans the page this cursor points at: its rows from the cursor, its width from `request`.
    /// A band taller than `request` allows is refused.
    pub fn plan(&self, request: &ReadRequest) -> Result<PagePlan, CommandError> {
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
        let width =
            max_columns.min(self.area.end.col - self.next.col + 1).min((max_cells / height).max(1));
        let last_col = self.next.col + width - 1;
        let page = RangeRef::new(self.next, CellRef::new(self.band_end_row, last_col));
        let next = if last_col < self.area.end.col {
            Some(ReadCursor { next: CellRef::new(self.next.row, last_col + 1), ..self.clone() })
        } else if self.band_end_row < self.area.end.row {
            let top = self.band_end_row + 1;
            Some(ReadCursor {
                next: CellRef::new(top, self.area.start.col),
                band_end_row: request.band_end(&self.area, top),
                ..self.clone()
            })
        } else {
            None
        };
        Ok((page, next))
    }
}

/// A page's cells, and the cursor after it.
pub type PagePlan = (RangeRef, Option<ReadCursor>);

impl fmt::Display for ReadCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let band = RangeRef::new(self.next, CellRef::new(self.band_end_row, self.next.col));
        let sheet = format_sheet_name(&self.sheet);
        write!(f, "{sheet}!{}@{}", self.area.a1(), band.a1())
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
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct ReadPage {
    pub sheet: String,
    /// The part of the range this page covers, like `A1:D200`; `null` when empty.
    pub range: Option<String>,
    /// Column letters, like `["A", "B", "C"]`.
    pub columns: Vec<String>,
    pub rows: Vec<ReadRow>,
    /// Where the next page starts, to pass back as `cursor`; `null` on the last page.
    pub next_cursor: Option<ReadCursor>,
    /// Cells whose text or formula was cut to keep the answer small.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<CutText>,
}

/// Most cells named in [`CutText::cells`].
pub const MAX_LISTED_CUT: usize = 20;

/// Text cut from an answer, and how to get the rest.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
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

/// `text` cut at `max` characters, or `None` when it is not longer.
pub fn cut_text(text: &str, max: usize) -> Option<String> {
    if text.len() <= max {
        return None;
    }
    let (end, _) = text.char_indices().nth(max)?;
    Some(text.get(..end).unwrap_or(text).to_owned())
}

/// Characters each text in a page of `cells` cells may keep, under [`MAX_RESPONSE_TEXT_CHARS`].
pub fn text_room(cells: u64) -> usize {
    let each = MAX_RESPONSE_TEXT_CHARS as u64 / cells.max(1);
    usize::try_from(each).unwrap_or(usize::MAX).clamp(16, MAX_CELL_TEXT_CHARS)
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

/// A cell value as JSON; an error as its text (`#DIV/0!`), an array as its top-left value.
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

/// `v` with text cut at `max` characters, counted in `cut`. An array gives its top-left value.
fn cut_value(v: &CellValue, max: usize, cut: &mut u64) -> CellValue {
    match v {
        CellValue::Text(t) => match cut_text(t, max) {
            Some(short) => {
                *cut += 1;
                CellValue::Text(short.into())
            }
            None => v.clone(),
        },
        CellValue::Array(a) => a.data.first().map(|v| cut_value(v, max, cut)).unwrap_or_default(),
        v => v.clone(),
    }
}

/// The part of `wanted` inside the used range, or `None`. Whole columns also lose the empty
/// rows above the data, and whole rows the empty columns left of it.
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

    /// How the cell at `at` shows in a page, cut at `max_chars`, and whether it was cut.
    fn cell_json(&self, at: CellRef, max_chars: usize, request: &ReadRequest) -> (Value, bool) {
        let mut cut = 0u64;
        let formula = self.sh.cell(at).and_then(|c| c.formula.as_ref()).map(|f| {
            let f = f.text.trim_start_matches('=');
            let short = cut_text(f, max_chars).inspect(|_| cut += 1);
            format!("={}", short.as_deref().unwrap_or(f))
        });
        let value = cut_value(&self.sh.value(at), max_chars, &mut cut);
        if value.is_empty() && formula.is_none() {
            return (Value::Null, false);
        }
        let shown = if request.detailed {
            let mut fields = Map::new();
            fields.insert("value".into(), value_json(&value));
            fields.insert("type".into(), json!(kind(&value)));
            if let Some(f) = formula {
                fields.insert("formula".into(), json!(f));
            }
            let format = self.wb.styles.get(self.sh.style_id(at)).num_fmt.as_str();
            if !format.eq_ignore_ascii_case("General") {
                fields.insert("format".into(), json!(format));
            }
            Value::Object(fields)
        } else {
            match (request.mode, formula) {
                (ReadMode::Values, _) | (ReadMode::Formulas, None) => value_json(&value),
                (ReadMode::Formulas, Some(f)) => Value::String(f),
                (ReadMode::Both, f) => json!({ "value": value_json(&value), "formula": f }),
            }
        };
        (shown, cut > 0)
    }
}

/// One page of a range of one sheet.
fn read_page(sheet: &View<'_>, request: &ReadRequest) -> Result<ReadPage, CommandError> {
    let name = &sheet.sh.name;
    let cursor = match &request.cursor {
        Some(cursor) => cursor.clone(),
        None => {
            let used = sheet.used_range();
            let area = match &request.range {
                Some(wanted) => used.and_then(|used| clip_to_used(wanted, used)),
                None => used,
            };
            let Some(area) = area else {
                return Ok(ReadPage { sheet: name.clone(), ..ReadPage::default() });
            };
            ReadCursor::start(name, area, request)
        }
    };
    let (page, next_cursor) = cursor.plan(request)?;
    let room = text_room(page.count());
    let mut cut = CutText { max_chars: room, ..CutText::default() };
    let sheet_name = format_sheet_name(name);
    let rows = (page.start.row..=page.end.row)
        .map(|r| ReadRow {
            row: r + 1,
            cells: (page.start.col..=page.end.col)
                .map(|c| {
                    let at = CellRef::new(r, c);
                    let (cell, was_cut) = sheet.cell_json(at, room, request);
                    if was_cut {
                        cut.add(format!("{sheet_name}!{at}"));
                    }
                    cell
                })
                .collect(),
        })
        .collect();
    Ok(ReadPage {
        sheet: name.clone(),
        range: Some(page.a1()),
        columns: (page.start.col..=page.end.col).map(col_to_letters).collect(),
        rows,
        next_cursor,
        cut: cut.finish(),
    })
}

/// Largest array a formula may return to `eval`.
const MAX_EVAL_CELLS: usize = 10_000;

impl Kernel {
    /// Sheets, used ranges and counts.
    pub fn overview(&self, book: &Book) -> Result<Overview, CommandError> {
        self.guard("reading the workbook", || {
            let wb = book.wb()?;
            let mut sheets = Vec::new();
            for sh in &wb.sheets {
                let mut sheet = SheetOverview {
                    name: sh.name.clone(),
                    used_range: View { wb, sh }.used_range().map(|used| used.a1()),
                    cells: 0,
                    formulas: 0,
                    errors: 0,
                };
                for (_, cell) in sh.cells.iter() {
                    sheet.formulas += usize::from(cell.formula.is_some());
                    sheet.cells += usize::from(cell.formula.is_some() || !cell.value.is_empty());
                    sheet.errors += usize::from(matches!(cell.value, CellValue::Error(_)));
                }
                sheets.push(sheet);
            }
            Ok(Overview {
                sheets,
                date_system: match wb.date_system {
                    DateSystem::D1904 => "1904",
                    _ => "1900",
                }
                .to_owned(),
                defined_names: wb.names.len(),
                macros: match book.kind {
                    SourceKind::Package => crate::save::loss::macros(&book.source, true),
                    // An .xlsb is a zip with the same part names.
                    SourceKind::Imported("xlsb") => crate::save::loss::macros(&book.source, false),
                    SourceKind::Imported(format) => Some(format!(
                        "not checked: Carrysheet cannot see macros in .{format} files; it never runs them"
                    )),
                    SourceKind::Csv => None,
                },
                warnings: book.warnings.iter().map(|w| escape(w, 200)).collect(),
            })
        })
    }

    /// One page of a range.
    pub fn read_range(&self, book: &Book, request: &ReadRequest) -> Result<ReadPage, CommandError> {
        self.guard("reading cells", || {
            let wb = book.wb()?;
            let sheet = match &request.cursor {
                Some(cursor) => Some(cursor.sheet.as_str()),
                None => request.range.as_ref().and_then(|r| r.sheet.as_deref()),
            };
            let sh = wb.sheets.get(book.sheet_index(sheet)?);
            let sh = sh.ok_or_else(|| internal("reading cells", "sheet index out of range"))?;
            read_page(&View { wb, sh }, request)
        })
    }

    /// Computes an en-US formula without changing the book; `sheet` (or the first) is the default.
    pub fn evaluate(
        &self,
        book: &Book,
        sheet: Option<&str>,
        formula: &str,
    ) -> Result<Evaluation, CommandError> {
        self.guard("computing the formula", || {
            let wb = book.wb()?;
            let si = book.sheet_index(sheet)?;
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
        })
    }
}

/// Sheets, used ranges and counts.
pub(crate) fn overview(
    kernel: &Kernel,
    args: &Map<String, Value>,
    bytes: Vec<u8>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let book = kernel.open_bytes(&path, bytes)?;
    let overview = kernel.overview(&book)?;
    let mut sheets = Vec::new();
    for s in overview.sheets.iter().take(MAX_LISTED_SHEETS) {
        let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
        v["name"] = json!(escape(&s.name, 64));
        sheets.push(v);
    }
    let mut data = Map::new();
    data.insert("file".into(), shown(&path));
    data.extend(to_map(&overview));
    // Replacing a field keeps its place in the answer.
    data.insert("sheets".into(), Value::Array(sheets));
    let total = overview.sheets.len();
    if total > MAX_LISTED_SHEETS {
        data.insert("sheets_total".into(), json!(total));
        data.insert(
            "hint".into(),
            json!(format!(
                "The answer was cut to stay small: {MAX_LISTED_SHEETS} of {total} sheet(s) listed. Read a sheet's first row with read and a range like 'Name'!1:1."
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
    // validate_args has checked the mode.
    let mode = args.get("mode").and_then(|m| ReadMode::deserialize(m).ok());
    let request = ReadRequest {
        range: text(args, "range").map(SheetRange::parse).transpose()?,
        cursor: text(args, "cursor").map(ReadCursor::parse).transpose()?,
        mode: mode.unwrap_or_default(),
        max_rows: number(args, "max_rows").unwrap_or(DEFAULT_PAGE_ROWS),
        max_columns: number(args, "max_columns").unwrap_or(DEFAULT_PAGE_COLUMNS),
        detailed: detailed(args),
        ..ReadRequest::default()
    };
    let book = kernel.open_bytes(&path, bytes)?;
    let page = kernel.read_range(&book, &request)?;
    let mut data = to_map(&page);
    data.insert("file".into(), shown(&path));
    if page.next_cursor.is_some() {
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
    let mut data = to_map(&result);
    data.insert("file".into(), shown(&path));
    Ok(data)
}

/// The fields of a result as a JSON object.
fn to_map(result: &impl Serialize) -> Map<String, Value> {
    match serde_json::to_value(result) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}
