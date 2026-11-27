//! An engine that keeps workbooks in memory, for tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::model::{CellValue, SheetRange, Workbook};

use super::{
    Engine, Evaluation, FileFormat, Overview, ReadPage, ReadRequest, pick_sheet, read_workbook,
    value_json,
};
use crate::commands::error::{CommandError, ErrorCode};
use crate::commands::paths::display_path;

/// An engine that keeps workbooks in memory, for tests. "Opening" a path
/// returns the workbook stored under it; "saving" stores a copy.
///
/// It evaluates only numbers, quoted text, `TRUE`/`FALSE`, a single cell
/// reference and `SUM` of one range. Anything else is reported as not
/// implemented.
#[derive(Debug, Default)]
pub struct MemoryEngine {
    files: Mutex<HashMap<PathBuf, (Workbook, FileFormat)>>,
}

impl MemoryEngine {
    pub fn new() -> Self {
        MemoryEngine::default()
    }

    /// Stores a workbook under a path, as if it were a file there.
    pub fn insert(&self, path: impl Into<PathBuf>, workbook: Workbook) {
        let path = path.into();
        let format = FileFormat::from_path(&path).unwrap_or(FileFormat::Xlsx);
        self.lock().insert(path, (workbook, format));
    }

    /// The workbook and format last saved under a path.
    pub fn saved(&self, path: &Path) -> Option<(Workbook, FileFormat)> {
        self.lock().get(path).cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, (Workbook, FileFormat)>> {
        // A panic while holding the lock leaves plain data behind; keep using it.
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Engine for MemoryEngine {
    type Book = Workbook;

    fn name(&self) -> &'static str {
        "memory"
    }

    fn open(&self, path: &Path) -> Result<Workbook, CommandError> {
        self.lock()
            .get(path)
            .map(|(wb, _)| wb.clone())
            .ok_or_else(|| {
                CommandError::new(
                    ErrorCode::FileNotFound,
                    format!("{} does not exist", display_path(path)),
                    "there is no file at that path",
                    "Check the path, or give the full path to the file.",
                )
            })
    }

    fn overview(&self, book: &Workbook) -> Result<Overview, CommandError> {
        Ok(Overview::of(book))
    }

    fn read_range(&self, book: &Workbook, request: &ReadRequest) -> Result<ReadPage, CommandError> {
        read_workbook(book, request)
    }

    fn evaluate(
        &self,
        book: &Workbook,
        sheet: Option<&str>,
        formula: &str,
    ) -> Result<Evaluation, CommandError> {
        let f = formula.trim();
        let f = f.strip_prefix('=').unwrap_or(f).trim();
        let value = eval_simple(book, sheet, f)?;
        Ok(Evaluation {
            formula: f.to_owned(),
            kind: value.kind().to_owned(),
            value: value_json(&value),
            cut: None,
        })
    }

    fn save(&self, book: &Workbook, path: &Path, format: FileFormat) -> Result<(), CommandError> {
        self.lock()
            .insert(path.to_path_buf(), (book.clone(), format));
        Ok(())
    }
}

/// The tiny formula subset [`MemoryEngine`] understands.
fn eval_simple(book: &Workbook, sheet: Option<&str>, f: &str) -> Result<CellValue, CommandError> {
    if let Ok(n) = f.parse::<f64>()
        && n.is_finite()
    {
        return Ok(CellValue::Number(n));
    }
    if f.eq_ignore_ascii_case("true") {
        return Ok(CellValue::Bool(true));
    }
    if f.eq_ignore_ascii_case("false") {
        return Ok(CellValue::Bool(false));
    }
    if let Some(inner) = f.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
        && !inner.replace("\"\"", "").contains('"')
    {
        return Ok(CellValue::Text(inner.replace("\"\"", "\"")));
    }
    let upper = f.to_ascii_uppercase();
    if upper.starts_with("SUM(") && f.ends_with(')') {
        let r = SheetRange::parse(f[4..f.len() - 1].trim())?;
        let s = pick_sheet(book, r.sheet.as_deref(), sheet)?;
        let total: f64 = s
            .cells_in(r.range)
            .filter_map(|(_, _, c)| match c.value {
                CellValue::Number(n) => Some(n),
                _ => None,
            })
            .sum();
        return Ok(CellValue::Number(total));
    }
    if let Ok(r) = SheetRange::parse(f)
        && r.range.is_single_cell()
    {
        let s = pick_sheet(book, r.sheet.as_deref(), sheet)?;
        return Ok(s
            .cell(r.range.start)
            .map(|c| c.value.clone())
            .unwrap_or_default());
    }
    Err(CommandError::new(
        ErrorCode::NotImplemented,
        format!("the test engine cannot compute {f}"),
        "the in-memory engine only handles numbers, \"text\", TRUE, FALSE, one cell and SUM(range)",
        "Use one of those forms; the real engine will compute every function.",
    ))
}
