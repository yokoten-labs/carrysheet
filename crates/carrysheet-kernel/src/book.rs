//! An open workbook: reading files, overview, paged reads and formulas.

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use carrysheet_commands::engine::save::clean_from_file;
use carrysheet_commands::engine::{
    CellInfo, Evaluation, Overview, ReadPage, ReadRequest, SheetOverview, SheetView, cut_text,
    read_page, requested_sheet, text_room, value_json,
};
use carrysheet_commands::paths::display_path;
use carrysheet_commands::{CommandError, ErrorCode};
use carrysheet_model::{CellValue, ErrorKind, Range};
use gridcraft_engine::core::{CellError, CellRef as GCell, DateSystem, Value};
use gridcraft_engine::model::{Sheet, Workbook};
use gridcraft_engine::{DocState, EngineError, Session};
use serde_json::json;

use crate::{MAX_FILE_BYTES, internal};

/// Extensions [`read_file`] accepts.
const OPENS: &[&str] = &["xlsx", "xlsm", "xltx", "xltm", "xls", "xlsb", "csv"];

/// Largest array a formula may return to `eval`.
const MAX_EVAL_CELLS: usize = 10_000;

/// What the opened file was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// An OOXML zip package (xlsx, xlsm, ...): parts can be compared on save.
    Package,
    Csv,
    /// A format GridCraft imports with losses it does not list (xls, xlsb).
    Imported(&'static str),
}

/// A workbook opened by the kernel, already recalculated.
pub struct Book {
    pub(crate) session: Session,
    /// The file's bytes, to compare a save with.
    pub(crate) source: Arc<Vec<u8>>,
    pub(crate) kind: SourceKind,
    /// What GridCraft skipped or repaired while reading, as it said it.
    pub(crate) warnings: Vec<String>,
}

impl std::fmt::Debug for Book {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Book")
            .field("kind", &self.kind)
            .field("bytes", &self.source.len())
            .finish_non_exhaustive()
    }
}

fn io_error(path: &Path, e: &std::io::Error) -> CommandError {
    let (code, why, fix) = match e.kind() {
        std::io::ErrorKind::NotFound => (
            ErrorCode::FileNotFound,
            "there is no file at that path".to_owned(),
            "Check the path, or give the full path to the workbook.",
        ),
        std::io::ErrorKind::PermissionDenied => (
            ErrorCode::PermissionDenied,
            "the system refused access".to_owned(),
            "Check the file's permissions, or close the program that has it locked.",
        ),
        _ => (
            ErrorCode::Io,
            e.to_string(),
            "Check that the path names a readable file.",
        ),
    };
    CommandError::new(
        code,
        format!("{} could not be read", display_path(path)),
        why,
        fix,
    )
}

/// Reads a workbook file, refusing formats and sizes the kernel does not take.
pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>, CommandError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !OPENS.contains(&ext.as_str()) {
        return Err(CommandError::new(
            ErrorCode::UnsupportedFormat,
            format!("{} is not a workbook Carrysheet opens", display_path(path)),
            "the extension is not one of .xlsx, .xlsm, .xltx, .xltm, .xls, .xlsb or .csv",
            "Give a spreadsheet file with one of those extensions.",
        ));
    }
    let meta = std::fs::metadata(path).map_err(|e| io_error(path, &e))?;
    if !meta.is_file() {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            format!("{} is not a file", display_path(path)),
            "the path names a folder or a device",
            "Give the path of a workbook file.",
        ));
    }
    let too_big = || {
        CommandError::new(
            ErrorCode::LimitExceeded,
            format!("{} is too large to open", display_path(path)),
            format!("Carrysheet opens files up to {} MB", MAX_FILE_BYTES >> 20),
            "Split the workbook, or save a copy with fewer sheets or rows.",
        )
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err(too_big());
    }
    let file = std::fs::File::open(path).map_err(|e| io_error(path, &e))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(path, &e))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_big());
    }
    Ok(bytes)
}

/// Turns GridCraft's open error (plain text) into one with a code. The
/// text can quote the file, so it is cleaned and cut.
fn open_error(path: &Path, e: &EngineError) -> CommandError {
    let text = e.to_string();
    if let EngineError::Internal(_, detail) = e {
        return internal("opening the file", detail);
    }
    let shown = display_path(path);
    let why = clean_from_file(&text, 200);
    if text.contains(gridcraft_engine::io::PASSWORD_REQUIRED) {
        return CommandError::new(
            ErrorCode::UnsupportedFormat,
            format!("{shown} is protected with a password"),
            why,
            "Save a copy without the password in the program that made it, then use that copy.",
        );
    }
    if text.contains("too large") {
        return CommandError::new(
            ErrorCode::LimitExceeded,
            format!("{shown} holds more than Carrysheet accepts"),
            why,
            "Split the workbook into smaller files.",
        );
    }
    CommandError::new(
        ErrorCode::DamagedFile,
        format!("{shown} could not be opened as a workbook"),
        why,
        "Check that this is the right file and that it was copied completely; a damaged file is not repaired.",
    )
}

impl Book {
    /// Parses and recalculates a file's bytes. `path` names it in messages
    /// and picks the format when the bytes do not say.
    pub(crate) fn from_bytes(path: &Path, bytes: Vec<u8>) -> Result<Book, CommandError> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "book.xlsx".to_owned());
        let sniffed = gridcraft_xlsx::sniff(&bytes);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        // GridCraft reads text named .xlsx as csv. An .xlsx that is not a
        // zip package is damaged or not what it says, so refuse it.
        if matches!(ext.as_str(), "xlsx" | "xlsm" | "xltx" | "xltm")
            && sniffed == gridcraft_xlsx::Format::Csv
        {
            return Err(CommandError::new(
                ErrorCode::DamagedFile,
                format!("{} could not be opened as a workbook", display_path(path)),
                format!("an .{ext} file is a zip package, and this one is not"),
                "Check that this is the right file and that it was copied completely; a damaged file is not repaired.",
            ));
        }
        let kind = match sniffed {
            gridcraft_xlsx::Format::Xlsx => SourceKind::Package,
            gridcraft_xlsx::Format::Csv => SourceKind::Csv,
            gridcraft_xlsx::Format::Xlsb => SourceKind::Imported("xlsb"),
            gridcraft_xlsx::Format::Ods => SourceKind::Imported("ods"),
            _ => SourceKind::Imported("xls"),
        };
        let (mut wb, warnings) =
            gridcraft_engine::io::open_bytes(&name, &bytes).map_err(|e| open_error(path, &e))?;
        // Relative links to other workbooks are rewritten when saving to
        // another folder.
        wb.source_dir = std::path::absolute(path)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        // Recalculates everything, so values read are computed, not cached.
        let doc = DocState::new(wb, None, name);
        let mut session = Session::new();
        session.add_document(doc);
        Ok(Book {
            session,
            source: Arc::new(bytes),
            kind,
            warnings,
        })
    }

    pub(crate) fn doc(&self) -> Result<&DocState, CommandError> {
        self.session
            .active()
            .ok_or_else(|| internal("reading the workbook", "no document is open"))
    }

    pub(crate) fn wb(&self) -> Result<&Workbook, CommandError> {
        Ok(self.doc()?.wb.as_ref())
    }

    /// A sheet's index by name (any case), or the first sheet.
    pub(crate) fn sheet_index(&self, name: Option<&str>) -> Result<usize, CommandError> {
        let wb = self.wb()?;
        match name {
            Some(n) => wb.sheet_index(n).ok_or_else(|| no_such_sheet(n)),
            None if wb.sheets.is_empty() => Err(no_such_sheet("(first sheet)")),
            None => Ok(0),
        }
    }

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
                    if matches!(cell.value, Value::Error(_)) {
                        errors += 1;
                    }
                }
                SheetOverview {
                    name: sh.name.clone(),
                    used_range: View { wb, sh }.used_range().map(|r| r.to_string()),
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
        let value = gridcraft_engine::calc::recalc::evaluate_expr(wb, si, GCell::new(0, 0), &expr);
        let mut cut = 0u64;
        let (value, kind, room) = match &value {
            Value::Array(a) => {
                if a.data.len() > MAX_EVAL_CELLS {
                    return Err(CommandError::new(
                        ErrorCode::LimitExceeded,
                        format!("the result has {} cells", a.data.len()),
                        format!("eval returns at most {MAX_EVAL_CELLS} cells"),
                        "Wrap the formula in one that sums it up, like SUM(...), ROWS(...) or INDEX(...,1,1).",
                    ));
                }
                let room = text_room(a.data.len() as u64);
                let rows: Vec<serde_json::Value> = a
                    .data
                    .chunks(a.cols.max(1))
                    .map(|row| {
                        row.iter()
                            .map(|v| value_json(&cell_value_cut(v, room, &mut cut)))
                            .collect()
                    })
                    .collect();
                (json!(rows), "array".to_owned(), room)
            }
            v => {
                let room = text_room(1);
                let v = cell_value_cut(v, room, &mut cut);
                (value_json(&v), v.kind().to_owned(), room)
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
                    carrysheet_commands::limits::MAX_CELL_TEXT_CHARS
                )
            }),
        })
    }
}

fn no_such_sheet(name: &str) -> CommandError {
    CommandError::new(
        ErrorCode::NoSuchSheet,
        format!("there is no sheet named \"{name}\""),
        "the workbook has no sheet with that name (names are compared without case)",
        "Run overview to see the sheet names.",
    )
}

/// GridCraft's error values as ours.
fn error_kind(e: CellError) -> ErrorKind {
    match e {
        CellError::Null => ErrorKind::Null,
        CellError::Div0 => ErrorKind::Div0,
        CellError::Value => ErrorKind::Value,
        CellError::Ref => ErrorKind::Ref,
        CellError::Name => ErrorKind::Name,
        CellError::Num => ErrorKind::Num,
        CellError::NA => ErrorKind::NA,
        CellError::GettingData => ErrorKind::GettingData,
        CellError::Spill => ErrorKind::Spill,
        CellError::Calc => ErrorKind::Calc,
        CellError::Circ => ErrorKind::Circ,
    }
}

/// [`cell_value`] with text cut at `max` characters (counted in `cut`),
/// without copying the whole text first.
fn cell_value_cut(v: &Value, max: usize, cut: &mut u64) -> CellValue {
    match v {
        Value::Text(t) => match cut_text(t, max) {
            Some(short) => {
                *cut += 1;
                CellValue::Text(short)
            }
            None => CellValue::Text(t.to_string()),
        },
        Value::Array(a) => a
            .data
            .first()
            .map(|v| cell_value_cut(v, max, cut))
            .unwrap_or_default(),
        v => cell_value(v),
    }
}

/// A GridCraft value as a cell value; an array shows its top-left value.
pub(crate) fn cell_value(v: &Value) -> CellValue {
    match v {
        Value::Empty => CellValue::Empty,
        Value::Number(n) => CellValue::Number(*n),
        Value::Text(t) => CellValue::Text(t.to_string()),
        Value::Bool(b) => CellValue::Bool(*b),
        Value::Error(e) => CellValue::Error(error_kind(*e)),
        Value::Array(a) => a.data.first().map(cell_value).unwrap_or_default(),
    }
}

/// A GridCraft range as ours, or `None` past the grid.
pub(crate) fn range(r: gridcraft_engine::core::RangeRef) -> Option<Range> {
    let a = carrysheet_model::CellRef::new(r.start.row, r.start.col).ok()?;
    let b = carrysheet_model::CellRef::new(r.end.row, r.end.col).ok()?;
    Some(Range::new(a, b))
}

/// One sheet, as reads see it.
pub(crate) struct View<'a> {
    pub wb: &'a Workbook,
    pub sh: &'a Sheet,
}

impl SheetView for View<'_> {
    fn name(&self) -> &str {
        &self.sh.name
    }

    fn used_range(&self) -> Option<Range> {
        // Cells with only a style do not count as used.
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        let mut grow = |row: u32, col: u32| {
            bounds = Some(match bounds {
                None => (row, col, row, col),
                Some((r0, c0, r1, c1)) => (r0.min(row), c0.min(col), r1.max(row), c1.max(col)),
            });
        };
        for (at, cell) in self.sh.cells.iter() {
            if cell.formula.is_some() || !cell.value.is_empty() {
                grow(at.row, at.col);
            }
        }
        for r in self.sh.spill_ranges.values() {
            grow(r.start.row, r.start.col);
            grow(r.end.row, r.end.col);
        }
        let (r0, c0, r1, c1) = bounds?;
        range(gridcraft_engine::core::RangeRef::new(
            GCell::new(r0, c0),
            GCell::new(r1, c1),
        ))
    }

    fn cell(&self, row: u32, col: u32) -> Option<CellInfo> {
        self.cell_cut(row, col, usize::MAX).map(|(info, _)| info)
    }

    fn cell_cut(&self, row: u32, col: u32, max_chars: usize) -> Option<(CellInfo, bool)> {
        let at = GCell::new(row, col);
        let mut cut = 0u64;
        let formula = self.sh.cell(at).and_then(|c| c.formula.as_ref()).map(|f| {
            let f = f.text.trim_start_matches('=');
            cut_text(f, max_chars)
                .inspect(|_| cut += 1)
                .unwrap_or_else(|| f.to_owned())
        });
        let value = cell_value_cut(&self.sh.value(at), max_chars, &mut cut);
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
