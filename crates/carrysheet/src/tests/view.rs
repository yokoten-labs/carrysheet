//! The reads, on the real kernel: each book is written as an .xlsx file by
//! GridCraft and opened again through [`Kernel::open_bytes`].

use crate::Kernel;
use crate::error::ErrorCode;
use crate::open::Book;
use crate::range::SheetRange;
use crate::view::{ReadCursor, ReadMode, ReadRequest, value_json};
use gridcraft_engine::core::{CellError, CellRef, Value as CellValue};
use gridcraft_engine::io::FileKind;
use gridcraft_engine::model::{Cell, Formula, Sheet, Workbook};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// A sheet named `name` holding `cells`, each at a 0-based row and column.
fn sheet(name: &str, cells: Vec<(u32, u32, CellValue)>) -> Arc<Sheet> {
    let mut sheet = Sheet::new(name);
    for (row, col, value) in cells {
        sheet.set_value(CellRef::new(row, col), value);
    }
    Arc::new(sheet)
}

/// The sheets as an .xlsx file, opened by the kernel.
fn open(sheets: Vec<Arc<Sheet>>) -> (Kernel, Book) {
    let mut wb = Workbook::new();
    wb.sheets = sheets;
    let bytes = gridcraft_engine::io::save_bytes_as(&wb, "book.xlsx", FileKind::Xlsx).unwrap();
    let kernel = Kernel::new();
    let book = kernel.open_bytes(Path::new("book.xlsx"), bytes).unwrap();
    (kernel, book)
}

fn engine() -> (Kernel, Book) {
    let mut cells: Vec<(u32, u32, CellValue)> = vec![(0, 0, "Item".into()), (0, 1, "Price".into())];
    for r in 1..=5 {
        cells.push((r, 0, format!("item {r}").into()));
        cells.push((r, 1, (f64::from(r) * 10.0).into()));
    }
    cells.push((7, 1, CellError::Div0.into()));
    let mut data = sheet("Data", cells);
    let total = Cell::formula(Formula::new("=SUM(B2:B6)"));
    Arc::make_mut(&mut data).set_cell(CellRef::new(6, 1), total);
    open(vec![data, sheet("Q1 売上", vec![(0, 0, 7.0.into())]), sheet("Empty", vec![])])
}

#[test]
fn non_finite_numbers_become_text() {
    assert_eq!(value_json(&CellValue::Number(f64::NAN)), json!("NaN"));
    assert_eq!(value_json(&CellValue::Empty), Value::Null);
}

#[test]
fn overview_counts() {
    let (e, b) = engine();
    let o = e.overview(&b).unwrap();
    assert_eq!(o.sheets.len(), 3);
    assert_eq!(o.sheets[0].used_range.as_deref(), Some("A1:B8"));
    assert_eq!((o.sheets[0].cells, o.sheets[0].formulas, o.sheets[0].errors), (14, 1, 1));
    assert_eq!(o.sheets[2].used_range, None);
    assert_eq!(o.date_system, "1900");
    let j = serde_json::to_value(&o).unwrap();
    assert_eq!(j["sheets"][1]["name"], "Q1 売上");
}

#[test]
fn read_pages_through_a_range() {
    let (e, b) = engine();
    let mut req = ReadRequest { max_rows: 3, ..ReadRequest::default() };
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
fn grid(rows: u32, cols: u32) -> (Kernel, Book) {
    let cells = (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (r, c, CellRef::new(r, c).a1().into())))
        .collect();
    open(vec![sheet("Grid", cells)])
}

/// Reads every page and returns the page ranges and every address seen.
fn read_all(e: &Kernel, b: &Book, mut req: ReadRequest) -> (Vec<String>, Vec<String>) {
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
    let req = ReadRequest { max_rows: 2, max_columns: 2, ..ReadRequest::default() };
    let (pages, mut seen) = read_all(&e, &b, req);
    assert_eq!(pages, ["A1:B2", "C1:D2", "E1:E2", "A3:B3", "C3:D3", "E3"]);
    seen.sort();
    let mut all: Vec<String> =
        (0..3).flat_map(|r| (0..5).map(move |c| CellRef::new(r, c).a1())).collect();
    all.sort();
    assert_eq!(seen, all, "every cell exactly once");
}

#[test]
fn default_pages_are_bounded_in_columns_and_cells() {
    let (e, b) = grid(260, 120);
    let p = e.read_range(&b, &ReadRequest::default()).unwrap();
    // 50 columns, and 200 rows of them make 10,000 cells.
    assert_eq!(p.range.as_deref(), Some("A1:AX200"));
    assert_eq!(p.next_cursor.unwrap().to_string(), "Grid!A1:DP260@AY1:AY200");

    // Fewer cells allowed: shorter bands.
    let req = ReadRequest { max_cells: 1_000, ..ReadRequest::default() };
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
        "Grid!A1:B2@Grid!A1",
    ] {
        let e = ReadCursor::parse(bad).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument, "{bad}");
        assert!(e.what.contains("cursor"), "{bad}: {}", e.what);
    }
    // A sheet name may hold '@'.
    let c = ReadCursor::parse("'a@b'!A1:C9@B4:B6").unwrap();
    assert_eq!(c.sheet, "a@b");
    let (page, next) = c.plan(&ReadRequest::default()).unwrap();
    assert_eq!(page.a1(), "B4:C6");
    assert_eq!(next.unwrap().to_string(), "'a@b'!A1:C9@A7:A9");
    assert_eq!(c.to_string(), "'a@b'!A1:C9@B4:B6");
    assert_eq!(serde_json::to_value(&c).unwrap(), json!("'a@b'!A1:C9@B4:B6"));
    // A band over a whole column reads back as written.
    let c = ReadCursor::parse("Grid!A1:B1048576@A1:A1048576").unwrap();
    assert_eq!(c.to_string(), "Grid!A:B@A:A");
    assert_eq!(ReadCursor::parse(&c.to_string()).unwrap(), c);

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
    assert_eq!(e.read_range(&b, &req).unwrap_err().code, ErrorCode::NoSuchSheet);
}

#[test]
fn read_whole_columns_is_clipped_to_used_range() {
    let (e, b) = engine();
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
    let cells = vec![(99, 2, 1.0.into()), (101, 3, 2.0.into())];
    let (e, b) = open(vec![sheet("Data", cells)]);
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
    let (e, b) = engine();
    let mut req = ReadRequest {
        range: Some(SheetRange::parse("B7").unwrap()),
        mode: ReadMode::Formulas,
        ..ReadRequest::default()
    };
    assert_eq!(e.read_range(&b, &req).unwrap().rows[0].cells[0], "=SUM(B2:B6)");
    req.mode = ReadMode::Both;
    let both = &e.read_range(&b, &req).unwrap().rows[0].cells[0];
    assert_eq!(both["formula"], "=SUM(B2:B6)");
    // The kernel computes the formula.
    assert_eq!(both["value"], json!(150.0));
    req.range = Some(SheetRange::parse("A2").unwrap());
    assert_eq!(e.read_range(&b, &req).unwrap().rows[0].cells[0]["formula"], Value::Null);
}

#[test]
fn read_empty_sheet_and_outside_used_range() {
    let (e, b) = engine();
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
    assert_eq!(e.read_range(&b, &req).unwrap_err().code, ErrorCode::NoSuchSheet);
    // A missing sheet's name is escaped: no terminal codes reach stderr.
    let range = Some(SheetRange::parse("'\u{1b}]52;c;eA==\u{7}'!A1").unwrap());
    let e = e.read_range(&b, &ReadRequest { range, ..ReadRequest::default() }).unwrap_err();
    assert!(!e.what.contains(['\u{1b}', '\u{7}']), "{}", e.what);
}
