use crate::model::reference::*;

fn cell(s: &str) -> CellRef {
    CellRef::parse(s).unwrap()
}

#[test]
fn column_letters_round_trip() {
    let cases = [
        (0, "A"),
        (1, "B"),
        (25, "Z"),
        (26, "AA"),
        (27, "AB"),
        (51, "AZ"),
        (52, "BA"),
        (701, "ZZ"),
        (702, "AAA"),
        (16_383, "XFD"),
    ];
    for (n, s) in cases {
        assert_eq!(column_to_letters(n), s, "{n}");
        assert_eq!(column_from_letters(s), Some(n), "{s}");
    }
    for n in 0..MAX_COLS {
        assert_eq!(column_from_letters(&column_to_letters(n)), Some(n));
    }
}

#[test]
fn column_letters_reject() {
    assert_eq!(column_from_letters(""), None);
    assert_eq!(column_from_letters("XFE"), None);
    assert_eq!(column_from_letters("ZZZ"), None);
    assert_eq!(column_from_letters("AAAA"), None);
    assert_eq!(column_from_letters("A1"), None);
    assert_eq!(column_from_letters("é"), None);
    assert_eq!(column_from_letters("xfd"), Some(16_383));
}

#[test]
fn cell_parse_basic() {
    let c = cell("B7");
    assert_eq!((c.row, c.col), (6, 1));
    assert!(!c.row_absolute && !c.col_absolute);
    assert_eq!(c.to_string(), "B7");
}

#[test]
fn cell_parse_absolute_forms() {
    for (s, col_abs, row_abs) in [
        ("$A$1", true, true),
        ("$A1", true, false),
        ("A$1", false, true),
        ("A1", false, false),
    ] {
        let c = cell(s);
        assert_eq!((c.col_absolute, c.row_absolute), (col_abs, row_abs), "{s}");
        assert_eq!(c.to_string(), s);
    }
}

#[test]
fn cell_parse_lower_case_and_leading_zeros() {
    assert_eq!(cell("xfd1").to_string(), "XFD1");
    assert_eq!(cell("a01").to_string(), "A1");
}

#[test]
fn cell_limits() {
    let last = cell("XFD1048576");
    assert_eq!((last.row, last.col), (MAX_ROWS - 1, MAX_COLS - 1));
    assert!(matches!(
        CellRef::parse("A1048577"),
        Err(RefError::RowOutOfRange { .. })
    ));
    assert!(matches!(
        CellRef::parse("XFE1"),
        Err(RefError::ColumnOutOfRange { .. })
    ));
    assert!(matches!(
        CellRef::parse("A0"),
        Err(RefError::RowOutOfRange { .. })
    ));
    assert!(matches!(
        CellRef::parse("A99999999999999999999"),
        Err(RefError::RowOutOfRange { .. })
    ));
    assert!(CellRef::new(MAX_ROWS, 0).is_err());
    assert!(CellRef::new(0, MAX_COLS).is_err());
    assert!(CellRef::new(MAX_ROWS - 1, MAX_COLS - 1).is_ok());
}

#[test]
fn cell_rejects_garbage() {
    for s in [
        "", "A", "1", "$", "$$A1", "A$$1", "1A", "A1B", "A 1", " A1", "A1 ", "A-1", "A1.5", "Ａ1",
        "売1", "$1", "A$",
    ] {
        assert!(CellRef::parse(s).is_err(), "{s:?} should fail");
    }
    assert_eq!(CellRef::parse(""), Err(RefError::Empty));
}

#[test]
fn cell_absolute_relative_helpers() {
    let c = cell("C3");
    assert_eq!(c.absolute().to_string(), "$C$3");
    assert_eq!(c.absolute().relative(), c);
    assert!(c.same_cell(&c.absolute()));
    assert_eq!("C3".parse::<CellRef>().unwrap(), c);
}

#[test]
fn range_single_cell() {
    let r = Range::parse("C3").unwrap();
    assert!(r.is_single_cell());
    assert_eq!(r.cell_count(), 1);
    assert_eq!(r.to_string(), "C3");
}

#[test]
fn range_cells() {
    let r = Range::parse("A1:C10").unwrap();
    assert_eq!(r.shape, RangeShape::Cells);
    assert_eq!((r.row_count(), r.col_count(), r.cell_count()), (10, 3, 30));
    assert_eq!(r.to_string(), "A1:C10");
    assert!(r.contains(0, 0) && r.contains(9, 2));
    assert!(!r.contains(10, 0) && !r.contains(0, 3));
}

#[test]
fn range_same_cell_twice_keeps_colon() {
    assert_eq!(Range::parse("A1:A1").unwrap().to_string(), "A1");
    assert_eq!(Range::parse("$A$1:A1").unwrap().to_string(), "$A$1:A1");
}

#[test]
fn range_normalizes_corners() {
    assert_eq!(Range::parse("C10:A1").unwrap().to_string(), "A1:C10");
    assert_eq!(Range::parse("A10:C1").unwrap().to_string(), "A1:C10");
    assert_eq!(Range::parse("$C$10:A1").unwrap().to_string(), "A1:$C$10");
    assert_eq!(Range::parse("C:A").unwrap().to_string(), "A:C");
    assert_eq!(Range::parse("5:2").unwrap().to_string(), "2:5");
}

#[test]
fn range_absolute() {
    let r = Range::parse("$A$1:$B$2").unwrap();
    assert!(r.start.row_absolute && r.end.col_absolute);
    assert_eq!(r.to_string(), "$A$1:$B$2");
}

#[test]
fn range_whole_columns() {
    let r = Range::parse("A:C").unwrap();
    assert_eq!(r.shape, RangeShape::Columns);
    assert_eq!(r.row_count(), MAX_ROWS);
    assert_eq!(r.col_count(), 3);
    assert_eq!(r.to_string(), "A:C");
    assert_eq!(Range::parse("$a:$c").unwrap().to_string(), "$A:$C");
    assert_eq!(Range::parse("XFD:XFD").unwrap().col_count(), 1);
    assert_eq!(Range::columns(2, 0).unwrap().to_string(), "A:C");
}

#[test]
fn hand_built_ranges_never_panic() {
    let (a1, c3) = (cell("A1"), cell("C3"));
    let reversed = Range {
        start: c3,
        end: a1,
        shape: RangeShape::Cells,
    };
    assert_eq!((reversed.row_count(), reversed.col_count()), (3, 3));
    assert_eq!(reversed.cell_count(), 9);
    assert!(reversed.contains(1, 1) && !reversed.contains(3, 0));
    assert_eq!(reversed.sorted(), Range::new(a1, c3));
    assert_eq!(
        reversed.intersect(&Range::parse("B2:D4").unwrap()),
        Some(Range::parse("B2:C3").unwrap())
    );
    assert!(Range::parse("D4").unwrap().intersect(&reversed).is_none());
    assert_eq!(reversed.to_string(), "C3:A1");

    let huge = CellRef {
        row: u32::MAX,
        col: u32::MAX,
        row_absolute: false,
        col_absolute: false,
    };
    let r = Range {
        start: a1,
        end: huge,
        shape: RangeShape::Cells,
    };
    assert_eq!(r.row_count(), u32::MAX);
    assert_eq!(r.col_count(), u32::MAX);
    assert!(r.contains(u32::MAX, 0));
    assert!(!r.to_string().is_empty());
}

#[test]
fn range_whole_rows() {
    let r = Range::parse("2:5").unwrap();
    assert_eq!(r.shape, RangeShape::Rows);
    assert_eq!((r.row_count(), r.col_count()), (4, MAX_COLS));
    assert_eq!(r.to_string(), "2:5");
    assert_eq!(
        Range::parse("$1:$1048576").unwrap().to_string(),
        "$1:$1048576"
    );
    assert_eq!(Range::rows(4, 1).unwrap().to_string(), "2:5");
    assert!(Range::rows(0, MAX_ROWS).is_err());
}

#[test]
fn whole_sheet_cell_count_does_not_overflow() {
    let r = Range::parse("A1:XFD1048576").unwrap();
    assert_eq!(r.cell_count(), u64::from(MAX_ROWS) * u64::from(MAX_COLS));
    assert_eq!(
        Range::parse("1:1048576").unwrap().cell_count(),
        r.cell_count()
    );
}

#[test]
fn range_rejects() {
    for s in [
        "",
        ":",
        "A1:",
        ":A1",
        "A1:B2:C3",
        "A:1",
        "A1:C",
        "1:A1",
        "A",
        "1",
        "A1:XFE1",
        "A1:A1048577",
        "Sheet1!A1",
        "A1;B2",
        "A1..B2",
        "A0:B2",
    ] {
        assert!(Range::parse(s).is_err(), "{s:?} should fail");
    }
    assert!(matches!(
        Range::parse("A1:XFE1"),
        Err(RefError::ColumnOutOfRange { input }) if input == "A1:XFE1"
    ));
}

#[test]
fn range_intersect() {
    let a = Range::parse("A1:C3").unwrap();
    let b = Range::parse("B2:D4").unwrap();
    assert_eq!(a.intersect(&b).unwrap().to_string(), "B2:C3");
    assert!(a.overlaps(&b));
    let c = Range::parse("E5:F6").unwrap();
    assert!(a.intersect(&c).is_none());
    let col = Range::parse("B:B").unwrap();
    assert_eq!(a.intersect(&col).unwrap().to_string(), "B1:B3");
}

#[test]
fn sheet_names_valid_and_invalid() {
    for ok in [
        "Sheet1",
        "売上",
        "Q1 売上",
        "It's",
        "a.b",
        "1",
        "データ_2026",
    ] {
        assert!(validate_sheet_name(ok).is_ok(), "{ok}");
    }
    let long = "x".repeat(32);
    for bad in [
        "",
        "a:b",
        "a/b",
        "a\\b",
        "a?",
        "a*",
        "[a]",
        "'a",
        "a'",
        "History",
        "history",
        long.as_str(),
        "a\u{7}",
    ] {
        assert!(validate_sheet_name(bad).is_err(), "{bad:?}");
    }
    // 31 CJK characters fit (one UTF-16 unit each); an emoji takes two.
    assert!(validate_sheet_name(&"表".repeat(31)).is_ok());
    assert!(validate_sheet_name(&"😀".repeat(16)).is_err());
}

#[test]
fn sheet_name_quoting() {
    for plain in ["Sheet1", "Data", "_x", "a.b", "Totals2026x"] {
        assert!(!sheet_name_needs_quotes(plain), "{plain}");
        assert_eq!(format_sheet_name(plain), plain);
    }
    for quoted in [
        "My sheet", "売上", "1st", ".a", "A1", "xfd1", "R1C1", "R", "C", "rc", "R2", "C10", "TRUE",
        "false", "a-b", "a!b", "It's",
    ] {
        assert!(sheet_name_needs_quotes(quoted), "{quoted}");
    }
    assert_eq!(format_sheet_name("It's"), "'It''s'");
    assert_eq!(format_sheet_name("Q1 売上"), "'Q1 売上'");
    // Names that only start like a reference stay plain.
    assert!(!sheet_name_needs_quotes("Report"));
    assert!(!sheet_name_needs_quotes("Cost"));
    assert!(!sheet_name_needs_quotes("ABCD1"));
}

#[test]
fn sheet_range_unqualified() {
    let r = SheetRange::parse("A1:B2").unwrap();
    assert_eq!(r.sheet, None);
    assert_eq!(r.to_string(), "A1:B2");
}

#[test]
fn sheet_range_plain_name() {
    let r = SheetRange::parse("Sheet1!$A$1:C3").unwrap();
    assert_eq!(r.sheet.as_deref(), Some("Sheet1"));
    assert_eq!(r.to_string(), "Sheet1!$A$1:C3");
}

#[test]
fn sheet_range_quoted_cjk() {
    let r = SheetRange::parse("'Q1 売上'!B2:D9").unwrap();
    assert_eq!(r.sheet.as_deref(), Some("Q1 売上"));
    assert_eq!(r.range.to_string(), "B2:D9");
    assert_eq!(r.to_string(), "'Q1 売上'!B2:D9");

    let r = SheetRange::parse("'売上'!A1").unwrap();
    assert_eq!(r.sheet.as_deref(), Some("売上"));
    assert_eq!(r.to_string(), "'売上'!A1");
}

#[test]
fn sheet_range_unquoted_cjk_is_accepted_and_quoted_on_output() {
    let r = SheetRange::parse("売上!A1").unwrap();
    assert_eq!(r.sheet.as_deref(), Some("売上"));
    assert_eq!(r.to_string(), "'売上'!A1");
    let r = SheetRange::parse("시트1!A:A").unwrap();
    assert_eq!(r.to_string(), "'시트1'!A:A");
}

#[test]
fn sheet_range_escaped_quote() {
    let r = SheetRange::parse("'It''s'!A1").unwrap();
    assert_eq!(r.sheet.as_deref(), Some("It's"));
    assert_eq!(r.to_string(), "'It''s'!A1");
    let r = SheetRange::parse("'a''''b'!A1").unwrap();
    assert_eq!(r.sheet.as_deref(), Some("a''b"));
    assert_eq!(r.to_string(), "'a''''b'!A1");
}

#[test]
fn sheet_range_quoted_with_bang_and_colon_like_text() {
    let r = SheetRange::parse("'a!b'!C3").unwrap();
    assert_eq!(r.sheet.as_deref(), Some("a!b"));
    assert_eq!(r.to_string(), "'a!b'!C3");
}

#[test]
fn sheet_range_round_trips() {
    for s in [
        "Sheet1!A1",
        "'My sheet'!A1:B2",
        "'Q1 売上'!$A$1:$C$3",
        "Data!A:C",
        "Data!2:5",
        "'1st'!A1",
        "'A1'!B2",
        "'It''s'!XFD1048576",
        "A1",
    ] {
        let r = SheetRange::parse(s).unwrap();
        assert_eq!(r.to_string(), s);
        assert_eq!(SheetRange::parse(&r.to_string()).unwrap(), r);
    }
}

#[test]
fn sheet_range_rejects() {
    for s in [
        "",
        "!A1",
        "''!A1",
        "'Sheet1!A1",
        "'Sheet1'A1",
        "'Sheet1'!",
        "Sheet1!",
        "My sheet!A1",
        "Sheet1:Sheet3!A1",
        "Sheet1!A1:B2:C3",
        "'a:b'!A1",
        "'History'!A1",
        "Sheet1!XFE1",
        "a-b!A1",
    ] {
        assert!(SheetRange::parse(s).is_err(), "{s:?} should fail");
    }
}

#[test]
fn sheet_range_errors_name_the_whole_input() {
    let err = SheetRange::parse("Sheet1!A0").unwrap_err();
    assert_eq!(
        err,
        RefError::RowOutOfRange {
            input: "Sheet1!A0".into()
        }
    );
    let msg = SheetRange::parse("My sheet!A1").unwrap_err().to_string();
    assert!(msg.contains("single quotes"), "{msg}");
    assert!(
        SheetRange::parse("Sheet1:Sheet3!A1")
            .unwrap_err()
            .to_string()
            .contains("several sheets")
    );
}

#[test]
fn error_messages_are_readable() {
    assert_eq!(RefError::Empty.to_string(), "the reference is empty");
    let msg = CellRef::parse("XFE1").unwrap_err().to_string();
    assert!(msg.contains("XFD"), "{msg}");
    let msg = CellRef::parse("A0").unwrap_err().to_string();
    assert!(msg.contains("1048576"), "{msg}");
}

#[test]
fn hostile_long_input_is_rejected_quickly() {
    let long = "A".repeat(100_000);
    assert!(CellRef::parse(&long).is_err());
    assert!(Range::parse(&format!("{long}:{long}")).is_err());
    let quoted = format!("'{}", "x".repeat(100_000));
    assert!(SheetRange::parse(&quoted).is_err());
}
