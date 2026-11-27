use crate::range::*;
use gridcraft_engine::core::{MAX_COLS, MAX_ROWS};

fn range(s: &str) -> SheetRange {
    SheetRange::parse(s).unwrap()
}

fn error(s: &str) -> String {
    SheetRange::parse(s).unwrap_err().to_string()
}

#[test]
fn cell_parse_basic() {
    let c = range("B7");
    assert_eq!((c.range.start.row, c.range.start.col), (6, 1));
    assert_eq!(c.to_string(), "B7");
}

#[test]
fn cell_parse_absolute_forms() {
    for s in ["$A$1", "$A1", "A$1", "A1"] {
        assert_eq!(range(s).to_string(), s);
    }
}

#[test]
fn cell_parse_lower_case_and_leading_zeros() {
    assert_eq!(range("xfd1").to_string(), "XFD1");
    assert_eq!(range("a01").to_string(), "A1");
}

#[test]
fn cell_limits() {
    let last = range("XFD1048576").range.end;
    assert_eq!((last.row, last.col), (MAX_ROWS - 1, MAX_COLS - 1));
    // More than 7 digits is no row, as in GridCraft, leading zeros or not.
    for s in ["A1048577", "A0", "A99999999999999999999", "A00000001"] {
        assert!(error(s).contains("has a row outside 1 to"), "{s}");
    }
    assert!(error("XFE1").contains("has a column past XFD"));
}

#[test]
fn cell_rejects_garbage() {
    for s in [
        "", "A", "1", "$", "$$A1", "A$$1", "1A", "A1B", "A 1", " A1", "A1 ", "A-1", "A1.5", "Ａ1",
        "売1", "$1", "A$",
    ] {
        assert!(SheetRange::parse(s).is_err(), "{s:?} should fail");
    }
    assert_eq!(error(""), "the reference is empty");
}

#[test]
fn range_single_cell() {
    let r = range("C3");
    assert!(r.range.is_single());
    assert_eq!(r.range.count(), 1);
    assert_eq!(r.to_string(), "C3");
}

#[test]
fn range_cells() {
    let r = range("A1:C10");
    assert_eq!(r.shape, Shape::Cells);
    let cells = r.range;
    assert_eq!((cells.height(), cells.width(), cells.count()), (10, 3, 30));
    assert_eq!(r.to_string(), "A1:C10");
    assert_eq!(cells_text(cells), "A1:C10");
}

#[test]
fn range_same_cell_twice_keeps_colon() {
    assert_eq!(range("A1:A1").to_string(), "A1");
    assert_eq!(range("$A$1:A1").to_string(), "$A$1:A1");
}

#[test]
fn range_normalizes_corners() {
    assert_eq!(range("C10:A1").to_string(), "A1:C10");
    assert_eq!(range("A10:C1").to_string(), "A1:C10");
    assert_eq!(range("$C$10:A1").to_string(), "A1:$C$10");
    assert_eq!(range("C:A").to_string(), "A:C");
    assert_eq!(range("5:2").to_string(), "2:5");
}

#[test]
fn range_absolute() {
    assert_eq!(range("$A$1:$B$2").to_string(), "$A$1:$B$2");
}

#[test]
fn range_whole_columns() {
    let r = range("A:C");
    assert_eq!(r.shape, Shape::Columns);
    assert_eq!(r.range.height(), MAX_ROWS);
    assert_eq!(r.range.width(), 3);
    assert_eq!(r.to_string(), "A:C");
    assert_eq!(range("$a:$c").to_string(), "$A:$C");
    assert_eq!(range("XFD:XFD").range.width(), 1);
}

#[test]
fn range_whole_rows() {
    let r = range("2:5");
    assert_eq!(r.shape, Shape::Rows);
    assert_eq!((r.range.height(), r.range.width()), (4, MAX_COLS));
    assert_eq!(r.to_string(), "2:5");
    assert_eq!(range("$1:$1048576").to_string(), "$1:$1048576");
}

#[test]
fn whole_sheet_cell_count_does_not_overflow() {
    let r = range("A1:XFD1048576").range;
    assert_eq!(r.count(), u64::from(MAX_ROWS) * u64::from(MAX_COLS));
    assert_eq!(range("1:1048576").range.count(), r.count());
    // Written as cells, it is shown as cells.
    assert_eq!(range("A1:XFD1048576").to_string(), "A1:XFD1048576");
    assert_eq!(cells_text(r), "A1:XFD1048576");
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
        "A1;B2",
        "A1..B2",
        "A0:B2",
        "+5:+7",
        "$$A:$$B",
        "Sheet1!A1!B2",
    ] {
        assert!(SheetRange::parse(s).is_err(), "{s:?} should fail");
    }
    assert_eq!(
        error("A1:XFE1"),
        "\"A1:XFE1\" has a column past XFD, the last of the 16384 columns"
    );
    assert!(error("A1:").starts_with("\"A1:\" is not a valid reference: "));
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
    let r = range("A1:B2");
    assert_eq!(r.sheet, None);
    assert_eq!(r.to_string(), "A1:B2");
}

#[test]
fn sheet_range_plain_name() {
    let r = range("Sheet1!$A$1:C3");
    assert_eq!(r.sheet.as_deref(), Some("Sheet1"));
    assert_eq!(r.to_string(), "Sheet1!$A$1:C3");
}

#[test]
fn sheet_range_quoted_cjk() {
    let r = range("'Q1 売上'!B2:D9");
    assert_eq!(r.sheet.as_deref(), Some("Q1 売上"));
    assert_eq!(r.range.to_string(), "B2:D9");
    assert_eq!(r.to_string(), "'Q1 売上'!B2:D9");

    let r = range("'売上'!A1");
    assert_eq!(r.sheet.as_deref(), Some("売上"));
    assert_eq!(r.to_string(), "'売上'!A1");
}

#[test]
fn sheet_range_unquoted_cjk_is_accepted_and_quoted_on_output() {
    let r = range("売上!A1");
    assert_eq!(r.sheet.as_deref(), Some("売上"));
    assert_eq!(r.to_string(), "'売上'!A1");
    let r = range("시트1!A:A");
    assert_eq!(r.to_string(), "'시트1'!A:A");
}

#[test]
fn sheet_range_escaped_quote() {
    let r = range("'It''s'!A1");
    assert_eq!(r.sheet.as_deref(), Some("It's"));
    assert_eq!(r.to_string(), "'It''s'!A1");
    let r = range("'a''''b'!A1");
    assert_eq!(r.sheet.as_deref(), Some("a''b"));
    assert_eq!(r.to_string(), "'a''''b'!A1");
}

#[test]
fn sheet_range_quoted_with_bang_and_colon_like_text() {
    let r = range("'a!b'!C3");
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
        let r = range(s);
        assert_eq!(r.to_string(), s);
        assert_eq!(range(&r.to_string()), r);
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
    assert_eq!(
        error("Sheet1!A0"),
        "\"Sheet1!A0\" has a row outside 1 to 1048576, the rows a sheet can have"
    );
    let msg = error("My sheet!A1");
    assert!(msg.contains("single quotes"), "{msg}");
    assert!(error("Sheet1:Sheet3!A1").contains("several sheets"));
    assert!(error("Sheet1!").contains("the range after '!' is empty"));
}

#[test]
fn error_messages_are_readable() {
    let msg = error("XFE1");
    assert!(msg.contains("XFD"), "{msg}");
    let msg = error("A0");
    assert!(msg.contains("1048576"), "{msg}");
}

#[test]
fn hostile_long_input_is_rejected_quickly() {
    let long = "A".repeat(100_000);
    assert!(SheetRange::parse(&long).is_err());
    assert!(SheetRange::parse(&format!("{long}:{long}")).is_err());
    let quoted = format!("'{}", "x".repeat(100_000));
    assert!(SheetRange::parse(&quoted).is_err());
}
