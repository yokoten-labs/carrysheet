//! Streams built in code: BIFF2, BIFF5 with many code pages, BIFF8 edge
//! cases, and hostile shapes the samples do not have.

mod common;

use carrysheet_biff::{
    BiffErrorKind, BiffVersion, CellError, CellValue, Container, Encryption, FormulaText, Limits,
    read, read_with_limits,
};
use common::*;

fn text(v: &CellValue) -> &str {
    match v {
        CellValue::Text(t) => t,
        other => panic!("expected text, got {other:?}"),
    }
}

fn formula(book: &carrysheet_biff::BiffWorkbook, sheet: usize, row: u32, col: u16) -> String {
    let cell = book.sheets[sheet].cell(row, col).expect("cell");
    match &cell.formula.as_ref().expect("formula").text {
        FormulaText::Parsed(s) => s.clone(),
        FormulaText::Unparsed { reason, .. } => panic!("unparsed: {reason}"),
    }
}

#[test]
fn biff2_cells_formats_and_formulas() {
    let book = read(&biff2_sheet()).unwrap();
    assert_eq!(book.version, BiffVersion::Biff2);
    assert_eq!(book.container, Container::Raw);
    assert!(book.date_1904);
    assert_eq!(book.codepage, Some(32769));
    let sheet = &book.sheets[0];
    let a1 = sheet.cell(0, 0).unwrap();
    assert_eq!(a1.value, CellValue::Number(7.0));
    assert_eq!(book.number_format(a1), "0.00");
    let b1 = sheet.cell(0, 1).unwrap();
    assert_eq!(book.number_format(b1), "m/d/yy");
    assert!(book.is_date(b1));
    assert_eq!(text(&sheet.cell(0, 2).unwrap().value), "café");
    assert_eq!(sheet.cell(1, 0).unwrap().value, CellValue::Bool(true));
    assert_eq!(
        sheet.cell(1, 1).unwrap().value,
        CellValue::Error(CellError::Div0)
    );
    let c2 = sheet.cell(1, 2).unwrap();
    assert_eq!((c2.value.clone(), c2.xf), (CellValue::Empty, 21));
    assert_eq!(sheet.cell(2, 0).unwrap().value, CellValue::Number(8.0));
    assert_eq!(formula(&book, 0, 2, 0), "A1+1");
    assert_eq!(text(&sheet.cell(2, 1).unwrap().value), "xy");
    assert_eq!(formula(&book, 0, 2, 1), "\"x\"&\"y\"");
    assert_eq!(sheet.columns.len(), 1);
    assert_eq!(sheet.columns[0].width, 0x0A00);
    assert_eq!(sheet.dimensions.unwrap().to_a1(), "A1:C4");
    assert_eq!(book.names[0].name, "Total");
    assert_eq!(book.names[0].formula.text.as_str(), Some("$A$1:$B$2"));
    let summary = &book.summaries()[0];
    assert_eq!(summary.formulas, 2);
    assert_eq!(summary.blanks, 1);
    assert_eq!(summary.dates, 1);
}

#[test]
fn biff5_code_pages() {
    use encoding_rs::*;
    let cases: &[(u16, &'static Encoding, &str)] = &[
        (936, GBK, "中文表格"),
        (950, BIG5, "中文表格"),
        (932, SHIFT_JIS, "日本語の表"),
        (949, EUC_KR, "한국어 표"),
        (1250, WINDOWS_1250, "Łódź žluťoučký"),
        (1251, WINDOWS_1251, "Привет"),
        (1252, WINDOWS_1252, "Ça coûte 5 €"),
        (1253, WINDOWS_1253, "Ελληνικά"),
        (1254, WINDOWS_1254, "İstanbul ğ"),
        (1255, WINDOWS_1255, "עברית"),
        (1256, WINDOWS_1256, "عربي"),
        (1257, WINDOWS_1257, "Ąžuolas"),
        (1258, WINDOWS_1258, "Đơn giá"),
        (10000, MACINTOSH, "Café ﬁ"),
        (32768, MACINTOSH, "naïve"),
    ];
    for &(cp, enc, s) in cases {
        let (bytes, _, bad) = enc.encode(s);
        assert!(!bad, "{s} does not fit code page {cp}");
        let name = enc.encode("表").0;
        let name = if enc.encode("表").2 {
            b"Sheet".to_vec()
        } else {
            name.into_owned()
        };
        let book = read(&biff5_book(cp, &name, &bytes)).unwrap_or_else(|e| panic!("{cp}: {e}"));
        assert_eq!(book.version, BiffVersion::Biff5);
        assert_eq!(book.codepage, Some(cp));
        let sheet = &book.sheets[0];
        assert_eq!(text(&sheet.cell(0, 0).unwrap().value), s, "code page {cp}");
        let expected_name = enc.decode_without_bom_handling(&name).0.into_owned();
        assert_eq!(sheet.name, expected_name, "code page {cp}");
        assert!(book.warnings.is_empty(), "{cp}: {:?}", book.warnings);
    }
}

#[test]
fn biff5_names_3d_refs_and_strings() {
    let book = read(&biff5_book(1252, b"Data Sheet", b"x")).unwrap();
    assert_eq!(
        book.names[0].formula.text.as_str(),
        Some("'Data Sheet'!$B$1")
    );
    assert_eq!(formula(&book, 0, 0, 2), "Rate*2");
    assert_eq!(formula(&book, 0, 1, 2), "SUM(A2:B2)&\"!\"");
    let sheet = &book.sheets[0];
    assert_eq!(text(&sheet.cell(1, 2).unwrap().value), "0!");
    let b1 = sheet.cell(0, 1).unwrap();
    assert_eq!(book.number_format(b1), "0.0%");
}

#[test]
fn unknown_code_page_falls_back_with_a_warning() {
    let book = read(&biff5_book(437, b"S", b"abc")).unwrap();
    assert_eq!(text(&book.sheets[0].cell(0, 0).unwrap().value), "abc");
    assert!(book.warnings.iter().any(|w| w.message.contains("437")));
}

#[test]
fn biff8_sst_split_across_continue() {
    let book = read(&biff8_split_sst()).unwrap();
    assert_eq!(book.version, BiffVersion::Biff8);
    let sheet = &book.sheets[0];
    assert_eq!(sheet.name, "表1");
    assert_eq!(text(&sheet.cell(0, 0).unwrap().value), "ABC中文");
    let b1 = sheet.cell(0, 1).unwrap();
    assert_eq!(text(&b1.value), "Hi");
    let runs = b1.runs.as_ref().expect("rich text runs");
    assert_eq!(runs.len(), 2);
    assert_eq!((runs[1].start, runs[1].font), (1, 2));
    assert_eq!(text(&sheet.cell(0, 2).unwrap().value), "xyz");
    assert_eq!(sheet.cell(1, 0).unwrap().value, CellValue::Number(1.0));
    assert_eq!(sheet.cell(1, 1).unwrap().value, CellValue::Number(-2.0));
    assert_eq!(sheet.cell(1, 2).unwrap().value, CellValue::Number(0.5));
    assert_eq!(sheet.cell(2, 1).unwrap().value, CellValue::Empty);
    assert_eq!(sheet.merged[0].to_a1(), "A4:B5");
    // The SST and its CONTINUE records are modeled, not kept raw.
    assert!(
        book.raw_records
            .iter()
            .all(|r| r.record_type != 0x00FC && r.record_type != 0x003C)
    );
}

#[test]
fn filepass_is_a_clear_error() {
    let mut s = Stream::default();
    s.rec(0x0809, &cat(&[&u16le(0x0600), &u16le(5), &[0u8; 12]]));
    s.rec(0x002F, &cat(&[&u16le(1), &u16le(2), &[0u8; 8]]));
    let err = read(&s.bytes).unwrap_err();
    assert_eq!(
        err.kind,
        BiffErrorKind::Encrypted {
            method: Encryption::Rc4CryptoApi
        }
    );
    let msg = err.to_string();
    assert!(
        msg.contains("encrypted with a password; decryption is not supported yet"),
        "{msg}"
    );
    assert!(msg.contains("at stream offset 20"), "{msg}");
}

#[test]
fn not_xls_inputs() {
    for bytes in [&b""[..], b"PK\x03\x04rest", b"hello world", &[0x09, 0x08]] {
        let err = read(bytes).unwrap_err();
        assert!(matches!(err.kind, BiffErrorKind::NotXls { .. }), "{err}");
    }
    // OLE2 signature with nothing behind it.
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.resize(600, 0);
    let err = read(&ole).unwrap_err();
    assert!(matches!(err.kind, BiffErrorKind::Container { .. }), "{err}");
}

#[test]
fn truncated_records_report_the_offset() {
    let mut bytes = biff2_sheet();
    bytes.truncate(bytes.len() - 7);
    let err = read(&bytes).unwrap_err();
    assert!(
        matches!(err.kind, BiffErrorKind::UnexpectedEnd { .. }),
        "{err}"
    );
    assert!(err.offset.is_some());
    // A NUMBER record with a short body.
    let mut s = Stream::default();
    s.rec(0x0209, &cat(&[&u16le(0x0003), &u16le(0x0010)]));
    s.rec(0x0203, &[0, 0, 0, 0, 0, 0, 1, 2]);
    s.rec(0x000A, &[]);
    let err = read(&s.bytes).unwrap_err();
    match err.kind {
        BiffErrorKind::Truncated {
            record,
            record_type,
            ..
        } => assert_eq!((record, record_type), ("NUMBER", 0x0203)),
        other => panic!("{other:?}"),
    }
    assert_eq!(err.offset, Some(18));
}

#[test]
fn limits_stop_hostile_counts() {
    // A long run of MULBLANK cells against a small cell limit.
    let mut s = Stream::default();
    s.rec(0x0809, &cat(&[&u16le(0x0600), &u16le(0x0010), &[0u8; 12]]));
    let mut body = cat(&[&u16le(0), &u16le(0)]);
    for _ in 0..200 {
        body.extend_from_slice(&u16le(0));
    }
    body.extend_from_slice(&u16le(199));
    s.rec(0x00BE, &body);
    s.rec(0x000A, &[]);
    let limits = Limits {
        max_cells: 100,
        ..Limits::default()
    };
    let err = read_with_limits(&s.bytes, &limits).unwrap_err();
    assert!(
        matches!(
            err.kind,
            BiffErrorKind::LimitExceeded { limit: "cells", .. }
        ),
        "{err}"
    );
    // An SST that claims four billion strings allocates nothing up front.
    let mut s = Stream::default();
    s.rec(0x0809, &cat(&[&u16le(0x0600), &u16le(0x0005), &[0u8; 12]]));
    s.rec(
        0x00FC,
        &cat(&[&u32le(u32::MAX), &u32le(u32::MAX), &u16le(1), &[0], b"a"]),
    );
    s.rec(0x000A, &[]);
    let book = read(&s.bytes).unwrap();
    assert_eq!(book.shared_strings.len(), 1);
    assert!(
        book.warnings
            .iter()
            .any(|w| w.message.contains("SST declares"))
    );
    // A CONTINUE chain longer than allowed.
    let mut s = Stream::default();
    s.rec(0x0809, &cat(&[&u16le(0x0600), &u16le(0x0005), &[0u8; 12]]));
    s.rec(0x00FC, &cat(&[&u32le(1), &u32le(1)]));
    for _ in 0..10 {
        s.rec(0x003C, &[]);
    }
    s.rec(0x000A, &[]);
    let limits = Limits {
        max_continues: 5,
        ..Limits::default()
    };
    let err = read_with_limits(&s.bytes, &limits).unwrap_err();
    assert!(
        matches!(err.kind, BiffErrorKind::LimitExceeded { .. }),
        "{err}"
    );
}

#[test]
fn unknown_records_are_kept_in_order() {
    let mut s = Stream::default();
    s.rec(0x0809, &cat(&[&u16le(0x0600), &u16le(0x0010), &[0u8; 12]]));
    s.rec(0x1234, b"abc");
    s.rec(
        0x0203,
        &cat(&[&u16le(0), &u16le(0), &u16le(0), &f64le(1.0)]),
    );
    s.rec(0x0099, b"");
    s.rec(0x000A, &[]);
    let book = read(&s.bytes).unwrap();
    let raw: Vec<_> = book.sheets[0]
        .raw_records
        .iter()
        .map(|r| (r.record_type, r.index, r.data.clone()))
        .collect();
    assert_eq!(
        raw,
        vec![
            (
                0x0809,
                0,
                cat(&[&u16le(0x0600), &u16le(0x0010), &[0u8; 12]])
            ),
            (0x1234, 1, b"abc".to_vec()),
            (0x0099, 3, vec![]),
        ]
    );
}

#[test]
fn unknown_formula_tokens_keep_the_bytes() {
    let mut s = Stream::default();
    s.rec(0x0809, &cat(&[&u16le(0x0600), &u16le(0x0010), &[0u8; 12]]));
    let rgce = [0x1E, 1, 0, 0xF0, 0x01];
    s.rec(
        0x0006,
        &cat(&[
            &u16le(0),
            &u16le(0),
            &u16le(0),
            &f64le(1.0),
            &u16le(0),
            &u32le(0),
            &u16le(rgce.len() as u16),
            &rgce,
        ]),
    );
    s.rec(0x000A, &[]);
    let book = read(&s.bytes).unwrap();
    let f = book.sheets[0].cell(0, 0).unwrap().formula.as_ref().unwrap();
    assert_eq!(f.tokens, rgce);
    match &f.text {
        FormulaText::Unparsed { reason, at } => {
            assert_eq!(*at, 3);
            assert!(reason.contains("0xF0"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(book.summaries()[0].unparsed_formulas, 1);
}

#[test]
fn biff4_bound_workbook() {
    let book = read(&biff4_workbook()).unwrap();
    assert_eq!(book.version, BiffVersion::Biff4);
    assert!(book.biff4_workbook);
    let names: Vec<_> = book.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Sales", "Costs £"]);
    for (i, sheet) in book.sheets.iter().enumerate() {
        assert_eq!(
            sheet.cell(0, 0).unwrap().value,
            CellValue::Number(10.0 * (i + 1) as f64)
        );
        assert_eq!(text(&sheet.cell(1, 0).unwrap().value), "£ 5");
    }
    assert!(book.warnings.is_empty(), "{:?}", book.warnings);
}
