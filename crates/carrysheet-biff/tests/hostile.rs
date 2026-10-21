//! Hostile shapes found in review: inputs that used to take minutes or
//! hundreds of megabytes, and arithmetic near u32::MAX. Each must finish
//! fast with bounded memory, or fail with a clear error.

mod common;

use std::time::{Duration, Instant};

use carrysheet_biff::{BiffErrorKind, CellValue, FormulaText, read};
use common::*;

/// Debug builds are slow; these bounds still catch the quadratic cases,
/// which took 35 to 110 seconds before the fixes.
const FAST: Duration = Duration::from_secs(3);

fn bof8(s: &mut Stream, dt: u16) {
    s.rec(
        0x0809,
        &cat(&[
            &u16le(0x0600),
            &u16le(dt),
            &u16le(0),
            &u16le(1997),
            &u32le(0),
            &u32le(6),
        ]),
    );
}

fn eof(s: &mut Stream) {
    s.rec(0x000A, &[]);
}

/// A BOUNDSHEET with a placeholder offset; returns where to patch it.
fn boundsheet(s: &mut Stream, name: &str) -> usize {
    let at = s.pos();
    s.rec(
        0x0085,
        &cat(&[&u32le(0), &[0, 0, name.len() as u8, 0], name.as_bytes()]),
    );
    at
}

fn patch(s: &mut Stream, bound_at: usize, offset: usize) {
    s.bytes[bound_at + 4..bound_at + 8].copy_from_slice(&(offset as u32).to_le_bytes());
}

/// A BIFF8 FORMULA record.
fn formula(s: &mut Stream, row: u16, col: u16, tokens: &[u8], flags: u16) {
    s.rec(
        0x0006,
        &cat(&[
            &u16le(row),
            &u16le(col),
            &u16le(0),
            &[0u8; 8],
            &u16le(flags),
            &u32le(0),
            &u16le(tokens.len() as u16),
            tokens,
        ]),
    );
}

fn ptg_exp(row: u16, col: u16) -> Vec<u8> {
    cat(&[&[0x01], &u16le(row), &u16le(col)])
}

/// A SHRFMLA record (BIFF8): RefU, reserved, cUse, cce, tokens.
fn shrfmla(s: &mut Stream, r1: u16, r2: u16, c1: u8, c2: u8, tokens: &[u8]) {
    s.rec(
        0x04BC,
        &cat(&[
            &u16le(r1),
            &u16le(r2),
            &[c1, c2, 0, 1],
            &u16le(tokens.len() as u16),
            tokens,
        ]),
    );
}

/// A one-sheet workbook whose sheet holds `body`, after `globals`.
fn one_sheet(globals: impl FnOnce(&mut Stream), body: impl FnOnce(&mut Stream)) -> Vec<u8> {
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    globals(&mut s);
    let b = boundsheet(&mut s, "Sheet1");
    eof(&mut s);
    let start = s.pos();
    patch(&mut s, b, start);
    bof8(&mut s, 0x0010);
    body(&mut s);
    eof(&mut s);
    s.bytes
}

fn text(f: &FormulaText) -> &str {
    match f {
        FormulaText::Parsed(s) => s,
        FormulaText::Unparsed { reason, .. } => panic!("unparsed: {reason}"),
    }
}

#[test]
fn many_boundsheets_on_one_substream_are_read_once() {
    // 200 BOUNDSHEETs pointing at one sheet of 20 records of 60,000 bytes:
    // this kept 239 MB from a 1.2 MB file.
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    let bounds: Vec<usize> = (0..200)
        .map(|i| boundsheet(&mut s, &format!("S{i}")))
        .collect();
    eof(&mut s);
    let start = s.pos();
    for b in bounds {
        patch(&mut s, b, start);
    }
    bof8(&mut s, 0x0010);
    let big = vec![0x41u8; 60_000];
    for _ in 0..20 {
        s.rec(0x1234, &big);
    }
    eof(&mut s);
    // A BOUNDSHEET pointing into the globals is refused too.
    let t = Instant::now();
    let book = read(&s.bytes).unwrap();
    assert!(t.elapsed() < FAST, "{:?}", t.elapsed());
    assert_eq!(book.sheets.len(), 200);
    let kept: usize = book
        .sheets
        .iter()
        .flat_map(|s| &s.raw_records)
        .map(|r| r.data.len())
        .sum();
    assert!(
        kept <= s.bytes.len(),
        "{kept} bytes kept from {}",
        s.bytes.len()
    );
    assert_eq!(book.sheets[0].raw_records.len(), 21);
    assert!(book.sheets[1..].iter().all(|s| s.raw_records.is_empty()));
    assert!(
        book.warnings
            .iter()
            .any(|w| w.message.contains("already read"))
    );
}

#[test]
fn a_boundsheet_into_the_globals_is_left_empty() {
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    boundsheet(&mut s, "Globals"); // offset 0: the globals' own BOF
    eof(&mut s);
    let book = read(&s.bytes).unwrap();
    assert_eq!(book.sheets.len(), 1);
    assert!(book.sheets[0].raw_records.is_empty());
    assert!(
        book.warnings
            .iter()
            .any(|w| w.message.contains("already read"))
    );
}

#[test]
fn sst_with_thousands_of_continues_is_linear() {
    // 500 parts of 8,224 bytes of empty strings. The remaining-bytes count
    // used to walk every part for every string.
    let parts = 500;
    let total = parts * 8224;
    let strings = (total - 8) / 3;
    let mut body = vec![0u8; total];
    body[0..4].copy_from_slice(&(strings as u32).to_le_bytes());
    body[4..8].copy_from_slice(&(strings as u32).to_le_bytes());
    let bytes = one_sheet(
        |s| {
            for (i, chunk) in body.chunks(8224).enumerate() {
                s.rec(if i == 0 { 0x00FC } else { 0x003C }, chunk);
            }
        },
        |_| {},
    );
    let t = Instant::now();
    let book = read(&bytes).unwrap();
    assert!(t.elapsed() < FAST, "{:?}", t.elapsed());
    assert!(book.shared_strings.len() > 1_000_000);
}

#[test]
fn missing_shared_formula_anchors_do_not_go_quadratic() {
    // 40,000 SHRFMLA records and 40,000 cells pointing at an anchor none
    // of them has: every cell used to scan every SHRFMLA twice (45 s).
    let bytes = one_sheet(
        |_| {},
        |s| {
            for _ in 0..40_000 {
                shrfmla(s, 60_000, 60_000, 200, 200, &[0x1E, 1, 0]);
            }
            let exp = ptg_exp(65_000, 250);
            for i in 0..40_000u16 {
                formula(s, i, 0, &exp, 8);
            }
        },
    );
    let t = Instant::now();
    let book = read(&bytes).unwrap();
    assert!(t.elapsed() < FAST, "{:?}", t.elapsed());
    let sheet = &book.sheets[0];
    assert_eq!(sheet.cells.len(), 40_000);
    let f = sheet.cells[0].formula.as_ref().unwrap();
    assert!(matches!(f.text, FormulaText::Unparsed { .. }));
    assert!(
        book.warnings
            .iter()
            .any(|w| w.message.contains("range checks"))
    );
}

#[test]
fn shared_formulas_index_by_anchor() {
    // Many shared formulas, each used by its own anchor: found by the
    // index, not by scanning.
    let bytes = one_sheet(
        |_| {},
        |s| {
            for i in 0..20_000u16 {
                formula(s, i, 0, &ptg_exp(i, 0), 8);
                shrfmla(s, i, i, 0, 0, &[0x1E, 7, 0]);
            }
        },
    );
    let t = Instant::now();
    let book = read(&bytes).unwrap();
    assert!(t.elapsed() < FAST, "{:?}", t.elapsed());
    let sheet = &book.sheets[0];
    assert!(
        sheet
            .cells
            .iter()
            .all(|c| text(&c.formula.as_ref().unwrap().text) == "7")
    );
    assert_eq!(sheet.shared_formulas[0].uses, 1);
}

#[test]
fn rendering_one_big_shared_formula_per_cell_is_bounded() {
    // 64 KB of PtgAttrSpace in one shared formula, used by 20,000 cells:
    // the text is tiny, but each cell re-reads the tokens.
    let mut tokens = Vec::new();
    while tokens.len() < 65_000 {
        tokens.extend_from_slice(&[0x19, 0x40, 0x00, 0x00]);
    }
    tokens.extend_from_slice(&[0x1E, 1, 0]);
    let bytes = one_sheet(
        |_| {},
        |s| {
            formula(s, 0, 0, &ptg_exp(0, 0), 8);
            shrfmla(s, 0, 19_999, 0, 0, &tokens);
            for i in 1..20_000u16 {
                formula(s, i, 0, &ptg_exp(0, 0), 8);
            }
        },
    );
    let t = Instant::now();
    let err = read(&bytes).unwrap_err();
    assert!(t.elapsed() < FAST, "{:?}", t.elapsed());
    assert!(
        matches!(err.kind, BiffErrorKind::LimitExceeded { .. }),
        "{err}"
    );
}

#[test]
fn supbook_url_with_a_huge_length_character() {
    // An external SUPBOOK whose path is U+0005 then U+10FFFF: the count
    // used to be taken as 1,114,111 characters, per SUPBOOK (65 s for 4000).
    let bytes = one_sheet(
        |s| {
            for _ in 0..4000 {
                s.rec(
                    0x01AE,
                    &cat(&[
                        &u16le(0),
                        &u16le(3),
                        &[1],
                        &u16le(5),
                        &u16le(0xDBFF),
                        &u16le(0xDFFF),
                    ]),
                );
            }
        },
        |_| {},
    );
    let t = Instant::now();
    read(&bytes).unwrap();
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
}

/// SUPBOOK (internal, `sheets` sheets) and EXTERNSHEET entries.
fn internal_refs(s: &mut Stream, sheets: u16, xti: &[(u16, u16, u16)]) {
    s.rec(0x01AE, &cat(&[&u16le(sheets), &u16le(0x0401)]));
    let mut body = u16le(xti.len() as u16).to_vec();
    for &(book, first, last) in xti {
        body.extend_from_slice(&cat(&[&u16le(book), &u16le(first), &u16le(last)]));
    }
    s.rec(0x0017, &body);
}

#[test]
fn ref_3d_in_a_shared_formula_is_relative_to_the_cell() {
    // [MS-XLS] RgceLocRel: in a shared formula, PtgRef3d holds offsets.
    // Two sheets; the shared formula on Sheet1 refers to Sheet2.
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    internal_refs(&mut s, 2, &[(0, 1, 1)]);
    let b1 = boundsheet(&mut s, "Sheet1");
    let b2 = boundsheet(&mut s, "Sheet2");
    eof(&mut s);
    let start = s.pos();
    patch(&mut s, b1, start);
    bof8(&mut s, 0x0010);
    // A1:A3 share Sheet2!A1 (relative row and column, offsets 0).
    let ref3d = cat(&[&[0x5A], &u16le(0), &u16le(0), &u16le(0xC000)]);
    // B1:B3 share an area: Sheet2!A1:B2, relative.
    let area3d = cat(&[
        &[0x5B],
        &u16le(0),
        &u16le(0),
        &u16le(1),
        &u16le(0xC000),
        &u16le(0xC001),
    ]);
    formula(&mut s, 0, 0, &ptg_exp(0, 0), 8);
    shrfmla(&mut s, 0, 2, 0, 0, &ref3d);
    formula(&mut s, 1, 0, &ptg_exp(0, 0), 8);
    formula(&mut s, 2, 0, &ptg_exp(0, 0), 8);
    formula(&mut s, 0, 1, &ptg_exp(0, 1), 8);
    shrfmla(&mut s, 0, 2, 1, 1, &area3d);
    formula(&mut s, 1, 1, &ptg_exp(0, 1), 8);
    formula(&mut s, 2, 1, &ptg_exp(0, 1), 8);
    eof(&mut s);
    let start = s.pos();
    patch(&mut s, b2, start);
    bof8(&mut s, 0x0010);
    eof(&mut s);

    let book = read(&s.bytes).unwrap();
    let sheet = &book.sheets[0];
    let f = |r: u32, c: u16| {
        text(&sheet.cell(r, c).unwrap().formula.as_ref().unwrap().text).to_string()
    };
    assert_eq!(
        [f(0, 0), f(1, 0), f(2, 0)],
        ["Sheet2!A1", "Sheet2!A2", "Sheet2!A3"]
    );
    assert_eq!(
        [f(0, 1), f(1, 1), f(2, 1)],
        ["Sheet2!B1:C2", "Sheet2!B2:C3", "Sheet2!B3:C4"]
    );
}

#[test]
fn ref_3d_in_a_biff5_shared_formula_is_relative_to_the_cell() {
    // BIFF5: ixals < 0 means this workbook, sheets itabFirst..itabLast.
    let mut s = Stream::default();
    s.rec(
        0x0809,
        &cat(&[
            &u16le(0x0500),
            &u16le(0x0005),
            &u16le(0x0DBB),
            &u16le(0x07CC),
        ]),
    );
    let b1 = s.pos();
    s.rec(0x0085, &cat(&[&u32le(0), &[0, 0, 6], b"Sheet1"]));
    let b2 = s.pos();
    s.rec(0x0085, &cat(&[&u32le(0), &[0, 0, 6], b"Sheet2"]));
    s.rec(0x0017, &cat(&[&[7, 0x03], b"Sheet2"]));
    eof(&mut s);
    let start = s.pos();
    patch(&mut s, b1, start);
    s.rec(
        0x0809,
        &cat(&[
            &u16le(0x0500),
            &u16le(0x0010),
            &u16le(0x0DBB),
            &u16le(0x07CC),
        ]),
    );
    // PtgRef3d (BIFF5): ixals, 8 reserved, itabFirst, itabLast, row word
    // (bit 15 row relative, bit 14 column relative), column byte.
    let ref3d = cat(&[
        &[0x5A],
        &(-1i16).to_le_bytes(),
        &[0u8; 8],
        &u16le(1),
        &u16le(1),
        &u16le(0xC000),
        &[0],
    ]);
    let f5 = |s: &mut Stream, row: u16, tokens: &[u8]| {
        s.rec(
            0x0006,
            &cat(&[
                &u16le(row),
                &u16le(0),
                &u16le(0),
                &[0u8; 8],
                &u16le(8),
                &u32le(0),
                &u16le(tokens.len() as u16),
                tokens,
            ]),
        );
    };
    let exp = cat(&[&[0x01], &u16le(0), &u16le(0)]);
    f5(&mut s, 0, &exp);
    s.rec(
        0x04BC,
        &cat(&[
            &u16le(0),
            &u16le(2),
            &[0, 0, 0, 3],
            &u16le(ref3d.len() as u16),
            &ref3d,
        ]),
    );
    f5(&mut s, 1, &exp);
    f5(&mut s, 2, &exp);
    eof(&mut s);
    let start = s.pos();
    patch(&mut s, b2, start);
    s.rec(
        0x0809,
        &cat(&[
            &u16le(0x0500),
            &u16le(0x0010),
            &u16le(0x0DBB),
            &u16le(0x07CC),
        ]),
    );
    eof(&mut s);

    let book = read(&s.bytes).unwrap();
    let sheet = &book.sheets[0];
    let got: Vec<String> = (0..3)
        .map(|r| text(&sheet.cell(r, 0).unwrap().formula.as_ref().unwrap().text).to_string())
        .collect();
    assert_eq!(got, ["Sheet2!A1", "Sheet2!A2", "Sheet2!A3"]);
}

#[test]
fn external_sheet_ranges_name_both_sheets() {
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    // External SUPBOOK: 3 sheets, path "\x01book.xls" (encoded, this
    // folder), then the sheet names.
    let path = "\u{1}book.xls";
    let mut sup = cat(&[&u16le(3), &u16le(path.len() as u16), &[0], path.as_bytes()]);
    for name in ["S1", "S2", "S3"] {
        sup.extend_from_slice(&cat(&[&u16le(2), &[0], name.as_bytes()]));
    }
    s.rec(0x01AE, &sup);
    s.rec(0x0017, &cat(&[&u16le(1), &u16le(0), &u16le(0), &u16le(2)]));
    let b = boundsheet(&mut s, "Sheet1");
    eof(&mut s);
    let start = s.pos();
    patch(&mut s, b, start);
    bof8(&mut s, 0x0010);
    formula(
        &mut s,
        0,
        0,
        &cat(&[&[0x3A], &u16le(0), &u16le(0), &u16le(0)]),
        0,
    );
    eof(&mut s);
    let book = read(&s.bytes).unwrap();
    let f = book.sheets[0].cell(0, 0).unwrap().formula.as_ref().unwrap();
    // S1 and S3 look like cell references, so Excel quotes the prefix.
    assert_eq!(text(&f.text), "'[book.xls]S1:S3'!$A$1");
}

#[test]
fn offsets_and_lengths_near_u32_max() {
    // A BOUNDSHEET offset of 0xFFFFFFFF (pos + 2 overflowed on 32-bit).
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    let b = boundsheet(&mut s, "Far");
    eof(&mut s);
    s.bytes[b + 4..b + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let book = read(&s.bytes).unwrap();
    assert_eq!(book.sheets.len(), 1);
    assert!(book.sheets[0].cells.is_empty());

    // An SST string whose ExtRst claims 0xFFFFFFFF bytes (the "needed" sum
    // in the error overflowed on 32-bit).
    let sst = cat(&[
        &u32le(1),
        &u32le(1),
        &u16le(1),
        &[0x04],
        &u32le(u32::MAX),
        b"x",
    ]);
    let bytes = one_sheet(
        |s| {
            s.rec(0x00FC, &sst);
        },
        |_| {},
    );
    let err = read(&bytes).unwrap_err();
    assert!(matches!(err.kind, BiffErrorKind::Truncated { .. }), "{err}");
    assert!(err.offset.is_some());
}

#[test]
fn inverted_merged_ranges_keep_the_record() {
    let bytes = one_sheet(
        |_| {},
        |s| {
            // A1:B2 (fine) and C3:A1 (end before start).
            s.rec(
                0x00E5,
                &cat(&[
                    &u16le(2),
                    &u16le(0),
                    &u16le(1),
                    &u16le(0),
                    &u16le(1),
                    &u16le(2),
                    &u16le(0),
                    &u16le(2),
                    &u16le(0),
                ]),
            );
        },
    );
    let book = read(&bytes).unwrap();
    let sheet = &book.sheets[0];
    assert_eq!(sheet.merged.len(), 1);
    assert!(sheet.raw_records.iter().any(|r| r.record_type == 0x00E5));
}

#[test]
fn biff2_formula_with_a_two_byte_length() {
    // Apache POI's reading of BIFF2 FORMULA: cce is two bytes. The record
    // length tells which layout a writer used.
    let mut s = Stream::default();
    s.rec(0x0009, &cat(&[&u16le(0x0002), &u16le(0x0010)]));
    let rgce = [0x1E, 42, 0];
    s.rec(
        0x0006,
        &cat(&[
            &u16le(0),
            &u16le(0),
            &[0, 0, 0],
            &f64le(42.0),
            &[0],
            &u16le(rgce.len() as u16),
            &rgce,
        ]),
    );
    s.rec(
        0x0006,
        &cat(&[
            &u16le(1),
            &u16le(0),
            &[0, 0, 0],
            &f64le(43.0),
            &[0],
            &[rgce.len() as u8],
            &[0x1E, 43, 0],
        ]),
    );
    eof(&mut s);
    let book = read(&s.bytes).unwrap();
    let sheet = &book.sheets[0];
    let a1 = sheet.cell(0, 0).unwrap();
    assert_eq!(a1.value, CellValue::Number(42.0));
    assert_eq!(text(&a1.formula.as_ref().unwrap().text), "42");
    assert_eq!(
        text(&sheet.cell(1, 0).unwrap().formula.as_ref().unwrap().text),
        "43"
    );
}

#[test]
fn operands_cannot_add_up_on_the_stack() {
    // A defined name 255 characters long, used 13,000 times in one formula
    // without operators: each operand is far under the 64 KB text limit,
    // but together they are 3.3 MB, held until the error about leftover
    // values. The stack as a whole is bounded now.
    let name = [b'N'; 255];
    let mut tokens = Vec::new();
    for _ in 0..13_000 {
        tokens.extend_from_slice(&[0x23, 1, 0, 0, 0]);
    }
    let bytes = one_sheet(
        |s| {
            s.rec(
                0x0018,
                &cat(&[
                    &u16le(0),
                    &[0, 255],
                    &u16le(3),
                    &u16le(0),
                    &u16le(0),
                    &[0, 0, 0, 0],
                    &[0],
                    &name,
                    &[0x1E, 1, 0],
                ]),
            );
        },
        |s| formula(s, 0, 0, &tokens, 0),
    );
    let book = read(&bytes).unwrap();
    let f = book.sheets[0].cell(0, 0).unwrap().formula.as_ref().unwrap();
    match &f.text {
        FormulaText::Unparsed { reason, at } => {
            assert!(reason.contains("longer than"), "{reason}");
            // Stopped after about 64 KB of operands, not at the end.
            assert!(*at < 2_000, "{at}");
        }
        _ => panic!("expected the text limit"),
    }
}
