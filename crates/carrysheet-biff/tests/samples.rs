//! Reads the Apache POI sample files and checks what they hold.

// The test reports what it compared with --nocapture; the workspace denies
// println! only so that library and MCP code never write to stdout.
#![allow(clippy::print_stdout)]

mod common;

use carrysheet_biff::{
    BiffErrorKind, BiffVersion, CellError, CellValue, Container, Encryption, FormulaKind,
    FormulaText, SheetKind, Visibility, read,
};
use common::*;

fn text(v: &CellValue) -> &str {
    match v {
        CellValue::Text(t) => t,
        other => panic!("expected text, got {other:?}"),
    }
}

fn formula_at(book: &carrysheet_biff::BiffWorkbook, sheet: &str, cell: &str) -> String {
    let (row, col) = parse_a1(cell);
    let s = book
        .sheet(sheet)
        .unwrap_or_else(|| panic!("no sheet {sheet}"));
    let c = s.cell(row, col).unwrap_or_else(|| panic!("no cell {cell}"));
    match &c
        .formula
        .as_ref()
        .unwrap_or_else(|| panic!("{cell} has no formula"))
        .text
    {
        FormulaText::Parsed(t) => t.clone(),
        FormulaText::Unparsed { reason, .. } => panic!("{cell}: unparsed: {reason}"),
    }
}

fn value_at(book: &carrysheet_biff::BiffWorkbook, sheet: &str, cell: &str) -> CellValue {
    let (row, col) = parse_a1(cell);
    let s = book
        .sheet(sheet)
        .unwrap_or_else(|| panic!("no sheet {sheet}"));
    s.cell(row, col)
        .map(|c| c.value.clone())
        .unwrap_or(CellValue::Empty)
}

fn parse_a1(a1: &str) -> (u32, u16) {
    let letters: String = a1.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits = &a1[letters.len()..];
    let col = letters
        .bytes()
        .fold(0u32, |acc, b| acc * 26 + (b - b'A' + 1) as u32)
        - 1;
    (digits.parse::<u32>().unwrap() - 1, col as u16)
}

#[test]
fn every_sample_reads_or_fails_cleanly() {
    let (mut read_ok, mut refused) = (0, 0);
    for path in sample_files() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).unwrap();
        match read(&bytes) {
            Ok(book) => {
                read_ok += 1;
                assert!(!book.sheets.is_empty(), "{name}: no sheets");
                for s in &book.sheets {
                    assert!(
                        s.cells
                            .windows(2)
                            .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col))
                    );
                    for c in &s.cells {
                        assert!((c.format as usize) < book.number_formats.len());
                    }
                }
            }
            Err(e) => {
                let expected = name.starts_with("clusterfuzz")
                    || [
                        "password.xls",
                        "xor-encryption-abc.xls",
                        // Damaged OLE2 containers from calamine's tests.
                        "OOM_alloc3.xls",
                        "issue_714.xls",
                        "too_small.xls",
                    ]
                    .contains(&name.as_str());
                assert!(expected, "{name}: {e}");
                refused += 1;
                println!("{name}: {e}");
                // Every error explains itself.
                assert!(!e.why().is_empty() && !e.fix().is_empty());
            }
        }
    }
    println!("{read_ok} samples read, {refused} refused as expected");
    assert!(read_ok >= 60, "only {read_ok} samples read");
}

#[test]
fn biff3_raw_stream() {
    let book = read(&sample("testEXCEL_3.xls")).unwrap();
    assert_eq!(book.version, BiffVersion::Biff3);
    assert_eq!(book.container, Container::Raw);
    assert_eq!(book.codepage, Some(32769));
    assert_eq!(book.sheets.len(), 1);
    let s = &book.sheets[0];
    assert_eq!(
        text(&s.cell(0, 0).unwrap().value),
        "Table C-13--Lemons: Utilization of production, by State, 1977/78 to date"
    );
    assert_eq!(formula_at(&book, "Sheet1", "C22"), "9090*76/2000");
    assert_eq!(value_at(&book, "Sheet1", "C22"), CellValue::Number(345.42));
    assert_eq!(book.names[1].name, "Print_Area_MI");
    assert_eq!(book.names[1].formula.text.as_str(), Some("$A$1:$J$36"));
    let summary = &book.summaries()[0];
    assert_eq!(
        (summary.cells, summary.formulas, summary.unparsed_formulas),
        (350, 39, 0)
    );
    assert_eq!(s.dimensions.unwrap().to_a1(), "A1:J35");
}

#[test]
fn biff4_raw_stream() {
    let book = read(&sample("testEXCEL_4.xls")).unwrap();
    assert_eq!(book.version, BiffVersion::Biff4);
    assert!(!book.biff4_workbook);
    assert_eq!(book.codepage, Some(1252));
    assert_eq!(formula_at(&book, "Sheet1", "B13"), "SUM(B15+B61+B63)");
    // C13/B13 is 0.0048; times 100 is the cached 0.48.
    assert_eq!(formula_at(&book, "Sheet1", "D13"), "(SUM(C13/B13))*100");
    let d13 = book.sheets[0].cell(12, 3).unwrap();
    assert_eq!(
        book.number_format(d13),
        "#,##0.00\"  \";\\-#,##0.00\"  \";\\ \\-\\-\"  \";@\"  \""
    );
    // A builtin name is stored as a one-character code.
    let print_area = book.names.iter().find(|n| n.builtin == Some(6)).unwrap();
    assert_eq!(print_area.name, "Print_Area");
    assert_eq!(print_area.formula.text.as_str(), Some("$A$1:$L$163"));
    let summary = &book.summaries()[0];
    assert_eq!(
        (summary.cells, summary.formulas, summary.unparsed_formulas),
        (1654, 157, 0)
    );
}

#[test]
fn biff5_book_stream() {
    for file in ["testEXCEL_5.xls", "testEXCEL_95.xls"] {
        let book = read(&sample(file)).unwrap();
        assert_eq!(book.version, BiffVersion::Biff5);
        assert_eq!(
            book.container,
            Container::Ole2 {
                stream: "Book".into()
            }
        );
        let names: Vec<_> = book.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Feuil1", "Feuil2", "Feuil3"]);
        assert_eq!(formula_at(&book, "Feuil1", "C6"), "+$B$6*$B$6");
        assert_eq!(formula_at(&book, "Feuil1", "B7"), "+$B$6+1");
        assert_eq!(value_at(&book, "Feuil1", "C7"), CellValue::Number(4.0));
        let e6 = book.sheets[0].cell(5, 4).unwrap();
        assert_eq!(book.number_format(e6), "0.00%");
    }
    // Excel 5 for the Mac: BOF version 0x0580, Mac Roman, one chart sheet.
    let book = read(&sample("64130.xls")).unwrap();
    assert_eq!(book.version, BiffVersion::Biff5);
    assert_eq!(book.codepage, Some(10000));
    assert_eq!(book.sheets[0].kind, SheetKind::Chart);
}

#[test]
fn biff8_basics() {
    let book = read(&sample("SampleSS.xls")).unwrap();
    assert_eq!(book.version, BiffVersion::Biff8);
    assert_eq!(
        book.container,
        Container::Ole2 {
            stream: "Workbook".into()
        }
    );
    assert_eq!(
        text(&value_at(&book, "First Sheet", "A1")),
        "Test spreadsheet"
    );
    assert_eq!(formula_at(&book, "Sheet Number 2", "D7"), "SUM(A7:C7)");
    assert_eq!(
        value_at(&book, "Sheet Number 2", "D7"),
        CellValue::Number(13.0)
    );
    let book = read(&sample("SimpleWithFormula.xls")).unwrap();
    assert_eq!(formula_at(&book, "Sheet1", "A3"), "CONCATENATE(A1,A2)");
    assert_eq!(text(&value_at(&book, "Sheet1", "A3")), "replacemereplaceme");
    let book = read(&sample("BOOK_in_capitals.xls")).unwrap();
    assert_eq!(
        book.container,
        Container::Ole2 {
            stream: "BOOK".into()
        }
    );
}

#[test]
fn date_systems() {
    let book = read(&sample("1904DateWindowing.xls")).unwrap();
    assert!(book.date_1904);
    let a1 = book.sheets[0].cell(0, 0).unwrap();
    assert!(book.is_date(a1));
    assert_eq!(book.number_format(a1), "m/d/yyyy");
    let book = read(&sample("1900DateWindowing.xls")).unwrap();
    assert!(!book.date_1904);
}

#[test]
fn east_asian_text() {
    let book = read(&sample("chinese-provinces.xls")).unwrap();
    assert_eq!(text(&value_at(&book, "provinces", "B2")), "安徽省");
    assert_eq!(text(&value_at(&book, "provinces", "B4")), "重庆市");
    let book = read(&sample("61287.xls")).unwrap();
    assert_eq!(book.sheets[0].name, "第一页");
    assert_eq!(book.sheets[0].merged.len(), 6);
    let book = read(&sample("unicodeNameRecord.xls")).unwrap();
    let name = book.names.iter().find(|n| n.name == "日本語").unwrap();
    assert_eq!(name.formula.text.as_str(), Some("Sheet1!$A$2:$A$4"));
    let book = read(&sample("DBCSSheetName.xls")).unwrap();
    assert_eq!(book.sheets[0].name, "जा");
}

#[test]
fn shared_and_array_formulas() {
    let book = read(&sample("SharedFormulaTest.xls")).unwrap();
    assert_eq!(formula_at(&book, "0", "B1"), "A$1*2");
    let cells_with_shared = book.sheets[0]
        .cells
        .iter()
        .filter(|c| {
            matches!(
                c.formula.as_ref().map(|f| f.kind),
                Some(FormulaKind::Shared { .. })
            )
        })
        .count();
    assert!(cells_with_shared > 10, "{cells_with_shared}");
    let book = read(&sample("overlapSharedFormula.xls")).unwrap();
    assert_eq!(formula_at(&book, "Sheet1", "A1"), "1+1");
    assert_eq!(formula_at(&book, "Sheet1", "G4"), "2+2");
    let book = read(&sample("testArraysAndTables.xls")).unwrap();
    assert_eq!(formula_at(&book, "Sheet1", "C2"), "{1.5,2.7,3.3,4.9,6.2}");
    assert_eq!(formula_at(&book, "Sheet1", "B4"), "{23;24;25;26;27;28}");
    assert_eq!(
        formula_at(&book, "Sheet1", "C11"),
        "{\"1A\",\"1B\",\"1C\";\"2A\",\"2B\",\"2C\"}"
    );
    assert_eq!(formula_at(&book, "Sheet1", "C16"), "{1,2,3,4}*{5,6,7,8}");
    assert_eq!(formula_at(&book, "Sheet1", "C27"), "TABLE(B26,)");
    assert_eq!(formula_at(&book, "Sheet1", "C33"), "TABLE(,B32)");
    assert_eq!(formula_at(&book, "Sheet1", "C41"), "TABLE(B38,B39)");
    let c2 = book.sheets[0].cell(1, 2).unwrap();
    match c2.formula.as_ref().unwrap().kind {
        FormulaKind::Array { range } => assert_eq!(range.to_a1(), "C2:G2"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn references_names_and_functions() {
    let book = read(&sample("3dFormulas.xls")).unwrap();
    assert_eq!(formula_at(&book, "Sheet1", "A7"), "SUM('Sh3'!A1:A4)");
    let book = read(&sample("55906-MultiSheetRefs.xls")).unwrap();
    assert_eq!(
        formula_at(&book, "Sheet1", "I6"),
        "COUNT(Sheet1:Sheet3!$A$1:$B$2)"
    );
    let book = read(&sample("testNames.xls")).unwrap();
    assert_eq!(formula_at(&book, "Sheet1", "A9"), "SUM(nr_B2_D3)/nr_A1");
    assert_eq!(formula_at(&book, "Sheet1", "A5"), "myFunc(\"def\")");
    let n = book.names.iter().find(|n| n.name == "nr_OFFSET").unwrap();
    assert_eq!(
        n.formula.text.as_str(),
        Some("OFFSET(Sheet1!$A$1,3,Sheet1!$A$2)")
    );
    let book = read(&sample("externalFunctionExample.xls")).unwrap();
    assert_eq!(formula_at(&book, "Sheet1", "A4"), "IF(ISEVEN(3),1.2,1.6)");
    let book = read(&sample("XRefCalc.xls")).unwrap();
    assert_eq!(
        formula_at(&book, "Sheet1", "E2"),
        "Cost*XRefCalcData.xls!Markup_Cost"
    );
    let book = read(&sample("49612.xls")).unwrap();
    assert_eq!(formula_at(&book, "Sheet1", "D1"), "SUM(BOB+JIM)");
    let book = read(&sample("IntersectionPtg.xls")).unwrap();
    assert_eq!(formula_at(&book, "Sheet1", "C5"), "SUM(A1:B2 B2:C3)");
    let book = read(&sample("missingFuncs44675.xls")).unwrap();
    assert_eq!(
        formula_at(&book, "funcs", "A1"),
        "DATEDIF(NOW(),NOW(),\"d\")"
    );
    let book = read(&sample("27349-vlookupAcrossSheets.xls")).unwrap();
    assert_eq!(
        formula_at(&book, "TEST", "A2"),
        "VLOOKUP(1,'DATA TABLE'!$A$8:'DATA TABLE'!$B$10,2)"
    );
}

#[test]
fn errors_and_booleans() {
    let book = read(&sample("ErrPtg.xls")).unwrap();
    for (cell, err, n) in [
        ("A1", "#NULL!", 1.0),
        ("A2", "#DIV/0!", 2.0),
        ("A7", "#N/A", 7.0),
    ] {
        assert_eq!(
            formula_at(&book, "Sheet1", cell),
            format!("ERROR.TYPE({err})")
        );
        assert_eq!(value_at(&book, "Sheet1", cell), CellValue::Number(n));
    }
    let book = read(&sample("AreaErrPtg.xls")).unwrap();
    let c = book.sheets[0]
        .cells
        .iter()
        .find(|c| c.formula.is_some())
        .unwrap();
    assert!(
        matches!(c.value, CellValue::Error(CellError::Ref)),
        "{:?}",
        c.value
    );
}

#[test]
fn broken_writers_are_tolerated() {
    // FORMAT records cut short by their writer: warnings, not failure.
    let book = read(&sample("61045_govdocs1_626534.xls")).unwrap();
    assert_eq!(book.warnings.len(), 2, "{:?}", book.warnings);
    assert_eq!(book.sheets[0].cells.len(), 232);
}

#[test]
fn embedded_charts_stay_raw() {
    let book = read(&sample("WithChart.xls")).unwrap();
    let chart_sheet = &book.sheets[1];
    assert!(
        chart_sheet
            .raw_records
            .iter()
            .any(|r| r.record_type == 0x1002)
    );
    assert!(chart_sheet.cells.is_empty());
    assert_eq!(book.sheets[0].visibility, Visibility::Visible);
}

#[test]
fn encrypted_files_are_refused() {
    for (file, method) in [
        ("password.xls", Encryption::Rc4),
        ("xor-encryption-abc.xls", Encryption::Xor),
    ] {
        let err = read(&sample(file)).unwrap_err();
        match err.kind {
            BiffErrorKind::Encrypted { method: m } => {
                // password.xls may use RC4 or CryptoAPI; both are RC4.
                if method == Encryption::Rc4 {
                    assert!(
                        matches!(m, Encryption::Rc4 | Encryption::Rc4CryptoApi),
                        "{m:?}"
                    );
                } else {
                    assert_eq!(m, method);
                }
            }
            other => panic!("{file}: {other:?}"),
        }
        assert!(err.to_string().contains("decryption is not supported yet"));
    }
}
