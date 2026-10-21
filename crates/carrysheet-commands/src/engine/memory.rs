//! An engine that keeps workbooks in memory, for tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use carrysheet_model::{CellValue, SheetRange, Workbook};

use super::{
    Engine, Evaluation, FileFormat, Overview, ReadPage, ReadRequest, pick_sheet, read_workbook,
    value_json,
};
use crate::error::{CommandError, ErrorCode};
use crate::paths::display_path;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ReadCursor, ReadMode};
    use carrysheet_model::{Cell, CellRef, ErrorKind};
    use serde_json::{Value, json};

    fn book() -> Workbook {
        let mut wb = Workbook::new();
        let s = wb.add_sheet("Data").unwrap();
        s.set(0, 0, Cell::value("Item")).unwrap();
        s.set(0, 1, Cell::value("Price")).unwrap();
        for r in 1..=5 {
            s.set(r, 0, Cell::value(format!("item {r}"))).unwrap();
            s.set(r, 1, Cell::value(f64::from(r) * 10.0)).unwrap();
        }
        s.set(6, 1, Cell::formula("SUM(B2:B6)")).unwrap();
        s.set(7, 1, Cell::value(ErrorKind::Div0)).unwrap();
        let s = wb.add_sheet("Q1 売上").unwrap();
        s.set(0, 0, Cell::value(7.0)).unwrap();
        wb.add_sheet("Empty").unwrap();
        wb
    }

    fn engine() -> MemoryEngine {
        let e = MemoryEngine::new();
        e.insert("book.xlsx", book());
        e
    }

    #[test]
    fn open_missing_file_is_a_clear_error() {
        let e = engine().open(Path::new("nope.xlsx")).unwrap_err();
        assert_eq!(e.code, ErrorCode::FileNotFound);
        assert!(e.what.contains("nope.xlsx"));
    }

    #[test]
    fn overview_counts() {
        let e = engine();
        let b = e.open(Path::new("book.xlsx")).unwrap();
        let o = e.overview(&b).unwrap();
        assert_eq!(o.sheets.len(), 3);
        assert_eq!(o.sheets[0].used_range.as_deref(), Some("A1:B8"));
        assert_eq!(
            (o.sheets[0].cells, o.sheets[0].formulas, o.sheets[0].errors),
            (14, 1, 1)
        );
        assert_eq!(o.sheets[2].used_range, None);
        assert_eq!(o.date_system, "1900");
        let j = serde_json::to_value(&o).unwrap();
        assert_eq!(j["sheets"][1]["name"], "Q1 売上");
    }

    #[test]
    fn read_pages_through_a_range() {
        let e = engine();
        let b = e.open(Path::new("book.xlsx")).unwrap();
        let mut req = ReadRequest {
            max_rows: 3,
            ..ReadRequest::default()
        };
        let p1 = e.read_range(&b, &req).unwrap();
        assert_eq!(p1.range.as_deref(), Some("A1:B3"));
        assert_eq!(p1.columns, ["A", "B"]);
        assert_eq!(p1.rows[0].cells, vec![json!("Item"), json!("Price")]);
        assert_eq!(p1.rows[2].row, 3);
        let next = p1.next_cursor.unwrap();
        assert_eq!(next.to_string(), "Data!A1:B8@A4:A6");

        req.cursor = Some(ReadCursor::parse(&next.to_string()).unwrap());
        let p2 = e.read_range(&b, &req).unwrap();
        assert_eq!(p2.range.as_deref(), Some("A4:B6"));
        req.cursor = p2.next_cursor;
        let p3 = e.read_range(&b, &req).unwrap();
        assert_eq!(p3.range.as_deref(), Some("A7:B8"));
        assert_eq!(p3.next_cursor, None);
        assert_eq!(p3.rows[1].cells[1], json!("#DIV/0!"));
    }

    /// A sheet `rows` by `cols` where each cell holds its own address.
    fn grid(rows: u32, cols: u32) -> (MemoryEngine, Workbook) {
        let e = MemoryEngine::new();
        let mut wb = Workbook::new();
        let s = wb.add_sheet("Grid").unwrap();
        for r in 0..rows {
            for c in 0..cols {
                let at = CellRef::new(r, c).unwrap().to_string();
                s.set(r, c, Cell::value(at)).unwrap();
            }
        }
        e.insert("grid.xlsx", wb);
        let b = e.open(Path::new("grid.xlsx")).unwrap();
        (e, b)
    }

    /// Reads every page and returns the page ranges and every address seen.
    fn read_all(
        e: &MemoryEngine,
        b: &Workbook,
        mut req: ReadRequest,
    ) -> (Vec<String>, Vec<String>) {
        let (mut pages, mut seen) = (Vec::new(), Vec::new());
        loop {
            let p = e.read_range(b, &req).unwrap();
            let cells: usize = p.rows.iter().map(|r| r.cells.len()).sum();
            assert!(p.rows.len() <= req.max_rows as usize);
            assert!(p.columns.len() <= req.max_columns as usize);
            assert!(cells <= req.max_cells as usize, "{cells} cells");
            pages.push(p.range.unwrap());
            for row in &p.rows {
                seen.extend(row.cells.iter().map(|v| v.as_str().unwrap().to_owned()));
            }
            // Through text, as an agent would pass it back.
            match p.next_cursor {
                Some(c) => req.cursor = Some(ReadCursor::parse(&c.to_string()).unwrap()),
                None => return (pages, seen),
            }
        }
    }

    #[test]
    fn wide_ranges_page_across_then_down() {
        let (e, b) = grid(3, 5);
        let req = ReadRequest {
            max_rows: 2,
            max_columns: 2,
            ..ReadRequest::default()
        };
        let (pages, mut seen) = read_all(&e, &b, req);
        assert_eq!(pages, ["A1:B2", "C1:D2", "E1:E2", "A3:B3", "C3:D3", "E3"]);
        seen.sort();
        let mut all: Vec<String> = (0..3)
            .flat_map(|r| (0..5).map(move |c| CellRef::new(r, c).unwrap().to_string()))
            .collect();
        all.sort();
        assert_eq!(seen, all, "every cell exactly once");
    }

    #[test]
    fn default_pages_are_bounded_in_columns_and_cells() {
        let (e, b) = grid(260, 120);
        let p = e.read_range(&b, &ReadRequest::default()).unwrap();
        // 50 columns, and 200 rows of them make 10,000 cells.
        assert_eq!(p.range.as_deref(), Some("A1:AX200"));
        assert_eq!(
            p.next_cursor.unwrap().to_string(),
            "Grid!A1:DP260@AY1:AY200"
        );

        // Fewer cells allowed: shorter bands.
        let req = ReadRequest {
            max_cells: 1_000,
            ..ReadRequest::default()
        };
        let p = e.read_range(&b, &req).unwrap();
        assert_eq!(p.range.as_deref(), Some("A1:AX20"));

        let req = ReadRequest {
            range: Some(SheetRange::parse("A1:J30").unwrap()),
            max_rows: 7,
            max_columns: 4,
            max_cells: 20,
            ..ReadRequest::default()
        };
        let (pages, seen) = read_all(&e, &b, req);
        assert_eq!(seen.len(), 300);
        assert_eq!(pages[0], "A1:D5");
    }

    #[test]
    fn cursors_are_checked() {
        for bad in [
            "",
            "Grid!A1:B2",
            "A1:B2@A1",
            "Grid!A1:B2@nope",
            "Grid!A1:B2@C1",
            "Grid!A1:B2@A1:B3",
            "Grid!A1:B2@A1:B1",
        ] {
            let e = ReadCursor::parse(bad).unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidArgument, "{bad}");
            assert!(e.what.contains("cursor"), "{bad}: {}", e.what);
        }
        // A sheet name may hold '@'.
        let c = ReadCursor::parse("'a@b'!A1:C9@B4:B6").unwrap();
        assert_eq!(c.sheet(), "a@b");
        assert_eq!(c.rest_of_band().to_string(), "B4:C6");
        assert_eq!(c.to_string(), "'a@b'!A1:C9@B4:B6");
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!("'a@b'!A1:C9@B4:B6")
        );

        // A band taller than the request allows is refused, not overrun.
        let (e, b) = grid(10, 2);
        let req = ReadRequest {
            cursor: Some(ReadCursor::parse("Grid!A1:B10@A1:A8").unwrap()),
            max_rows: 4,
            ..ReadRequest::default()
        };
        let err = e.read_range(&b, &req).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.fix.contains("max_rows"), "{}", err.fix);
        // A cursor on a sheet that is gone.
        let req = ReadRequest {
            cursor: Some(ReadCursor::parse("Gone!A1:B10@A1:A8").unwrap()),
            ..ReadRequest::default()
        };
        assert_eq!(
            e.read_range(&b, &req).unwrap_err().code,
            ErrorCode::NoSuchSheet
        );
    }

    #[test]
    fn read_whole_columns_is_clipped_to_used_range() {
        let e = engine();
        let b = e.open(Path::new("book.xlsx")).unwrap();
        let req = ReadRequest {
            range: Some(SheetRange::parse("Data!B:B").unwrap()),
            ..ReadRequest::default()
        };
        let p = e.read_range(&b, &req).unwrap();
        assert_eq!(p.range.as_deref(), Some("B1:B8"));
        assert_eq!(p.rows.len(), 8);
        assert_eq!(p.next_cursor, None);
    }

    #[test]
    fn whole_columns_and_rows_skip_the_empty_edges() {
        let e = MemoryEngine::new();
        let mut wb = Workbook::new();
        let s = wb.add_sheet("Data").unwrap();
        s.set(99, 2, Cell::value(1.0)).unwrap();
        s.set(101, 3, Cell::value(2.0)).unwrap();
        e.insert("far.xlsx", wb);
        let b = e.open(Path::new("far.xlsx")).unwrap();
        let read = |range: &str| {
            let req = ReadRequest {
                range: Some(SheetRange::parse(range).unwrap()),
                ..ReadRequest::default()
            };
            e.read_range(&b, &req).unwrap()
        };
        // Whole columns start at the first used row, not row 1.
        let p = read("C:D");
        assert_eq!(p.range.as_deref(), Some("C100:D102"));
        assert_eq!(p.rows.len(), 3);
        // Whole rows start at the first used column, not column A.
        let p = read("100:102");
        assert_eq!(p.range.as_deref(), Some("C100:D102"));
        // A cell range keeps the start it asked for.
        assert_eq!(read("B99:Z200").range.as_deref(), Some("B99:D102"));
        // Past the data.
        assert!(read("E:F").rows.is_empty());
        assert!(read("200:300").rows.is_empty());
    }

    #[test]
    fn read_modes() {
        let e = engine();
        let b = e.open(Path::new("book.xlsx")).unwrap();
        let mut req = ReadRequest {
            range: Some(SheetRange::parse("B7").unwrap()),
            mode: ReadMode::Formulas,
            ..ReadRequest::default()
        };
        assert_eq!(
            e.read_range(&b, &req).unwrap().rows[0].cells[0],
            "=SUM(B2:B6)"
        );
        req.mode = ReadMode::Both;
        let both = &e.read_range(&b, &req).unwrap().rows[0].cells[0];
        assert_eq!(both["formula"], "=SUM(B2:B6)");
        assert_eq!(both["value"], Value::Null);
        req.range = Some(SheetRange::parse("A2").unwrap());
        assert_eq!(
            e.read_range(&b, &req).unwrap().rows[0].cells[0]["formula"],
            Value::Null
        );
        assert_eq!(ReadMode::parse("both"), Some(ReadMode::Both));
        assert_eq!(ReadMode::parse("all"), None);
    }

    #[test]
    fn read_empty_sheet_and_outside_used_range() {
        let e = engine();
        let b = e.open(Path::new("book.xlsx")).unwrap();
        let req = ReadRequest {
            range: Some(SheetRange::parse("Empty!A1:C3").unwrap()),
            ..ReadRequest::default()
        };
        let p = e.read_range(&b, &req).unwrap();
        assert!(p.rows.is_empty() && p.range.is_none());
        let req = ReadRequest {
            range: Some(SheetRange::parse("Data!Z100").unwrap()),
            ..ReadRequest::default()
        };
        assert!(e.read_range(&b, &req).unwrap().rows.is_empty());
        let req = ReadRequest {
            range: Some(SheetRange::parse("Nope!A1").unwrap()),
            ..ReadRequest::default()
        };
        assert_eq!(
            e.read_range(&b, &req).unwrap_err().code,
            ErrorCode::NoSuchSheet
        );
    }

    #[test]
    fn evaluate_simple_formulas() {
        let e = engine();
        let b = e.open(Path::new("book.xlsx")).unwrap();
        let v = |f: &str, s: Option<&str>| e.evaluate(&b, s, f).unwrap();
        assert_eq!(v("=1.5", None).value, json!(1.5));
        assert_eq!(v("TRUE", None).value, json!(true));
        assert_eq!(v("\"a\"\"b\"", None).value, json!("a\"b"));
        assert_eq!(v("=SUM(B2:B6)", None).value, json!(150.0));
        assert_eq!(v("=sum(Data!B:B)", None).value, json!(150.0));
        assert_eq!(v("B3", None).value, json!(20.0));
        assert_eq!(v("A1", Some("Q1 売上")).value, json!(7.0));
        assert_eq!(v("'Q1 売上'!A1", None).kind, "number");
        assert_eq!(v("Z99", None).kind, "empty");
        assert_eq!(v("B8", None).value, json!("#DIV/0!"));
        let err = e.evaluate(&b, None, "VLOOKUP(1,A:B,2,0)").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotImplemented);
        let err = e.evaluate(&b, Some("Nope"), "A1").unwrap_err();
        assert_eq!(err.code, ErrorCode::NoSuchSheet);
        let err = e.evaluate(&b, None, "SUM(A1:XFE1)").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidReference);
    }

    #[test]
    fn save_stores_a_copy() {
        let e = engine();
        let b = e.open(Path::new("book.xlsx")).unwrap();
        e.save(&b, Path::new("out.csv"), FileFormat::Csv).unwrap();
        let (saved, format) = e.saved(Path::new("out.csv")).unwrap();
        assert_eq!(format, FileFormat::Csv);
        assert_eq!(saved, b);
        assert_eq!(e.name(), "memory");
    }
}
