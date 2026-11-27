use crate::model::reference::{CellRef, MAX_COLS, Range};
use crate::model::reference::{MAX_ROWS, RangeShape};
use crate::model::value::CellValue;
use crate::model::value::ErrorKind;
use crate::model::workbook::*;

fn r(s: &str) -> Range {
    Range::parse(s).unwrap()
}

#[test]
fn sheet_set_get_clear() {
    let mut s = Sheet::new("Data").unwrap();
    s.set(0, 0, Cell::value(1.0)).unwrap();
    s.set(2, 3, Cell::value("x")).unwrap();
    assert_eq!(s.get(0, 0).unwrap().value, CellValue::Number(1.0));
    assert_eq!(
        s.cell(CellRef::parse("D3").unwrap()).unwrap().value.kind(),
        "text"
    );
    assert_eq!(s.cell_count(), 2);
    s.set(2, 3, Cell::default()).unwrap();
    assert_eq!(s.cell_count(), 1);
    assert!(s.clear(0, 0).is_some());
    assert!(s.used_range().is_none());
}

#[test]
fn sheet_rejects_out_of_range_cells() {
    let mut s = Sheet::new("Data").unwrap();
    assert!(s.set(MAX_ROWS, 0, Cell::value(1.0)).is_err());
    assert!(s.set(0, MAX_COLS, Cell::value(1.0)).is_err());
    assert!(s.set(MAX_ROWS - 1, MAX_COLS - 1, Cell::value(1.0)).is_ok());
}

#[test]
fn used_range_and_cells_in() {
    let mut s = Sheet::new("Data").unwrap();
    s.set(1, 2, Cell::value(1.0)).unwrap();
    s.set(4, 0, Cell::value(2.0)).unwrap();
    s.set(3, 5, Cell::formula("=A1+1")).unwrap();
    assert_eq!(s.used_range().unwrap().to_string(), "A2:F5");
    let inside: Vec<_> = s.cells_in(r("B1:F4")).map(|(r, c, _)| (r, c)).collect();
    assert_eq!(inside, vec![(1, 2), (3, 5)]);
    assert_eq!(s.get(3, 5).unwrap().formula.as_deref(), Some("A1+1"));
}

#[test]
fn hand_built_reversed_ranges_do_not_panic() {
    let mut s = Sheet::new("Data").unwrap();
    s.set(1, 2, Cell::value(1.0)).unwrap();
    s.set(3, 5, Cell::value(2.0)).unwrap();
    let sorted = r("B1:F4");
    let reversed = Range {
        start: sorted.end,
        end: sorted.start,
        shape: sorted.shape,
    };
    let inside: Vec<_> = s.cells_in(reversed).map(|(r, c, _)| (r, c)).collect();
    assert_eq!(inside, vec![(1, 2), (3, 5)]);
    s.merge(reversed).unwrap();
    assert_eq!(s.merged_ranges()[0].to_string(), "B1:F4");
    assert!(s.merge(r("C2")).is_ok());
    assert!(s.merge(r("A1:B2")).is_err());
    let far = Range {
        start: CellRef::new(0, 0).unwrap(),
        end: CellRef {
            row: u32::MAX,
            col: u32::MAX,
            row_absolute: false,
            col_absolute: false,
        },
        shape: RangeShape::Cells,
    };
    assert_eq!(s.cells_in(far).count(), 2);
}

#[test]
fn cells_iterate_row_major() {
    let mut s = Sheet::new("Data").unwrap();
    s.set(1, 0, Cell::value(3.0)).unwrap();
    s.set(0, 1, Cell::value(2.0)).unwrap();
    s.set(0, 0, Cell::value(1.0)).unwrap();
    let order: Vec<_> = s.cells().map(|(r, c, _)| (r, c)).collect();
    assert_eq!(order, vec![(0, 0), (0, 1), (1, 0)]);
}

#[test]
fn number_format_and_error_values() {
    let c = Cell::value(0.25).with_number_format("0%");
    assert_eq!(c.number_format.as_deref(), Some("0%"));
    let e = Cell::value(ErrorKind::Div0);
    assert_eq!(e.value.to_string(), "#DIV/0!");
    assert!(!Cell::default().with_number_format("0").is_blank());
}

#[test]
fn merges() {
    let mut s = Sheet::new("Data").unwrap();
    s.merge(r("A1:B2")).unwrap();
    s.merge(r("C1")).unwrap();
    assert_eq!(s.merged_ranges().len(), 1);
    assert!(matches!(
        s.merge(r("B2:C3")),
        Err(ModelError::OverlappingMerge { .. })
    ));
    s.merge(r("C3:D4")).unwrap();
    assert_eq!(s.unmerge(CellRef::parse("A1").unwrap()), Some(r("A1:B2")));
    assert_eq!(s.merged_ranges(), &[r("C3:D4")]);
    assert!(s.unmerge(CellRef::parse("A1").unwrap()).is_none());
}

#[test]
fn column_widths() {
    let mut s = Sheet::new("Data").unwrap();
    assert_eq!(s.column_width(0), None);
    s.set_column_width(0, Some(12.5)).unwrap();
    s.set_column_width(3, Some(0.0)).unwrap();
    assert_eq!(s.column_width(0), Some(12.5));
    assert_eq!(s.column_widths().count(), 2);
    assert!(s.set_column_width(1, Some(f64::NAN)).is_err());
    assert!(s.set_column_width(1, Some(256.0)).is_err());
    assert!(s.set_column_width(MAX_COLS, Some(1.0)).is_err());
    s.set_column_width(0, None).unwrap();
    assert_eq!(s.column_width(0), None);
}

#[test]
fn workbook_sheets_case_insensitive() {
    let mut wb = Workbook::new();
    wb.add_sheet("売上").unwrap();
    wb.add_sheet("Data").unwrap();
    assert!(matches!(
        wb.add_sheet("DATA"),
        Err(ModelError::DuplicateSheet(_))
    ));
    assert_eq!(wb.sheet_index("data"), Some(1));
    assert_eq!(wb.sheet("売上").unwrap().name(), "売上");
    wb.sheet_mut("data")
        .unwrap()
        .set(0, 0, Cell::value(true))
        .unwrap();
    assert_eq!(wb.sheets()[1].cell_count(), 1);
    assert!(wb.add_sheet("bad/name").is_err());
}

#[test]
fn workbook_default_sheet_and_date_system() {
    let wb = Workbook::with_default_sheet();
    assert_eq!(wb.sheets()[0].name(), "Sheet1");
    assert_eq!(wb.date_system, DateSystem::Excel1900);
    assert_eq!(DateSystem::Excel1904.as_str(), "1904");
}

#[test]
fn rename_sheet() {
    let mut wb = Workbook::new();
    wb.add_sheet("A").unwrap();
    wb.add_sheet("B").unwrap();
    wb.rename_sheet("a", "Alpha").unwrap();
    assert_eq!(wb.sheets()[0].name(), "Alpha");
    // Changing only the case of a name is allowed.
    wb.rename_sheet("alpha", "ALPHA").unwrap();
    assert!(matches!(
        wb.rename_sheet("ALPHA", "b"),
        Err(ModelError::DuplicateSheet(_))
    ));
    assert!(matches!(
        wb.rename_sheet("nope", "x"),
        Err(ModelError::NoSuchSheet(_))
    ));
    assert!(wb.rename_sheet("B", "").is_err());
}

#[test]
fn remove_sheet_fixes_name_scopes() {
    let mut wb = Workbook::new();
    for n in ["A", "B", "C"] {
        wb.add_sheet(n).unwrap();
    }
    let name = |n: &str, sheet| DefinedName {
        name: n.into(),
        refers_to: "1".into(),
        sheet,
        hidden: false,
    };
    wb.defined_names = vec![name("g", None), name("b", Some(1)), name("c", Some(2))];
    wb.remove_sheet("B").unwrap();
    let names: Vec<_> = wb
        .defined_names
        .iter()
        .map(|n| (n.name.as_str(), n.sheet))
        .collect();
    assert_eq!(names, vec![("g", None), ("c", Some(1))]);
    assert!(wb.remove_sheet("B").is_err());
}

#[test]
fn defined_name_lookup_prefers_local() {
    let mut wb = Workbook::new();
    wb.add_sheet("A").unwrap();
    wb.defined_names.push(DefinedName {
        name: "Rate".into(),
        refers_to: "A!$B$1".into(),
        sheet: None,
        hidden: false,
    });
    wb.defined_names.push(DefinedName {
        name: "rate".into(),
        refers_to: "A!$C$1".into(),
        sheet: Some(0),
        hidden: false,
    });
    assert_eq!(wb.defined_name("RATE", None).unwrap().refers_to, "A!$B$1");
    assert_eq!(
        wb.defined_name("RATE", Some(0)).unwrap().refers_to,
        "A!$C$1"
    );
    assert!(wb.defined_name("other", Some(0)).is_none());
}

#[test]
fn unknown_parts_are_kept() {
    let mut wb = Workbook::with_default_sheet();
    wb.unknown_parts.push(UnknownPart {
        origin: "xl/vbaProject.bin".into(),
        content_type: Some("application/vnd.ms-office.vbaProject".into()),
        data: vec![1, 2, 3],
    });
    let copy = wb.clone();
    assert_eq!(copy.unknown_parts, wb.unknown_parts);
}

#[test]
fn formula_strips_leading_equals_once() {
    assert_eq!(Cell::formula("=1+1").formula.as_deref(), Some("1+1"));
    assert_eq!(
        Cell::formula("SUM(A:A)").formula.as_deref(),
        Some("SUM(A:A)")
    );
    assert_eq!(Cell::formula("==1").formula.as_deref(), Some("=1"));
}
