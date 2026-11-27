//! Hostile shapes built in code, ported from the tests of the readers that
//! left with the cut (BIFF, csv, encryption) and new ones for XLSB. Each
//! once took minutes or hundreds of megabytes in a reader of ours. On the
//! open path each must open or fail with a three-part error, within the
//! command's budget, and never panic.

use carrysheet::commands::ErrorCode;

use crate::biff::*;
use crate::support::{
    Depth, EXPIRY, ISSUE, Input, Outcome, Report, Waiver, check_waivers, mentions,
};
use crate::{cfb, xlsb};

/// Known failures of this file's cases. Each needs a reason, an issue and
/// the expiry `v0.1.0`; a waiver that no case needs fails the test.
const WAIVERS: &[Waiver] = &[
    Waiver {
        case: "biff2 sheet, raw",
        problem: "not refused with its version",
        reason: "GridCraft reads an .xls that is not a compound file as csv text",
        issue: Some(ISSUE),
        expires: EXPIRY,
    },
    Waiver {
        case: "biff4 workbook, raw",
        problem: "not refused with its version",
        reason: "GridCraft reads an .xls that is not a compound file as csv text",
        issue: Some(ISSUE),
        expires: EXPIRY,
    },
    Waiver {
        case: "a raw BIFF8 stream without a container",
        problem: "opened as text",
        reason: "GridCraft reads an .xls that is not a compound file as csv text",
        issue: Some(ISSUE),
        expires: EXPIRY,
    },
    Waiver {
        case: "text named .xls",
        problem: "opened as text",
        reason: "GridCraft reads an .xls that is not a compound file as csv text",
        issue: Some(ISSUE),
        expires: EXPIRY,
    },
];

#[test]
fn waivers_are_complete() {
    check_waivers(WAIVERS);
}

/// Runs each built case as an `.xls`, `.csv`, `.xlsx` or `.xlsb` file.
fn run_all(what: &str, cases: Vec<(&str, &'static str, Vec<u8>)>) -> Report {
    let mut report = Report::default();
    for (case, file_name, bytes) in cases {
        let outcome = report.run(case.to_owned(), Input::Bytes(file_name, bytes), Depth::Save);
        if let Some(outcome) = outcome {
            println!("{case} ({:?}): {}", report.last, describe(&outcome));
        }
    }
    println!("{}", report.summary(what));
    report
}

fn describe(outcome: &Outcome) -> String {
    match outcome.refused() {
        Some(e) => format!("refused ({}): {} / {}", e.code.as_str(), e.what, e.why),
        None => "opened".to_owned(),
    }
}

fn xls(stream: Vec<u8>) -> Vec<u8> {
    cfb::workbook(&stream)
}

fn many_boundsheets_on_one_substream() -> Vec<u8> {
    // 200 BOUNDSHEETs pointing at one sheet of 20 records of 60,000 bytes.
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
    s.bytes
}

fn boundsheet_into_the_globals() -> Vec<u8> {
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    boundsheet(&mut s, "Globals");
    eof(&mut s);
    s.bytes
}

fn sst_with_thousands_of_continues() -> Vec<u8> {
    // 150 parts of 8,224 bytes of empty strings, 411,000 strings: the
    // removed reader once walked every part for every string.
    let parts = 150;
    let total = parts * 8224;
    let strings = (total - 8) / 3;
    let mut body = vec![0u8; total];
    body[0..4].copy_from_slice(&(strings as u32).to_le_bytes());
    body[4..8].copy_from_slice(&(strings as u32).to_le_bytes());
    one_sheet(
        |s| {
            for (i, chunk) in body.chunks(8224).enumerate() {
                s.rec(if i == 0 { 0x00FC } else { 0x003C }, chunk);
            }
        },
        |_| {},
    )
}

fn missing_shared_formula_anchors() -> Vec<u8> {
    // 40,000 SHRFMLA records and 40,000 cells pointing at an anchor none
    // of them has.
    one_sheet(
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
    )
}

fn shared_formulas_each_at_its_anchor() -> Vec<u8> {
    one_sheet(
        |_| {},
        |s| {
            for i in 0..20_000u16 {
                formula(s, i, 0, &ptg_exp(i, 0), 8);
                shrfmla(s, i, i, 0, 0, &[0x1E, 7, 0]);
            }
        },
    )
}

fn one_big_shared_formula_in_many_cells() -> Vec<u8> {
    // 64 KB of PtgAttrSpace in one shared formula, used by 20,000 cells.
    let mut tokens = Vec::new();
    while tokens.len() < 65_000 {
        tokens.extend_from_slice(&[0x19, 0x40, 0x00, 0x00]);
    }
    tokens.extend_from_slice(&[0x1E, 1, 0]);
    one_sheet(
        |_| {},
        |s| {
            formula(s, 0, 0, &ptg_exp(0, 0), 8);
            shrfmla(s, 0, 19_999, 0, 0, &tokens);
            for i in 1..20_000u16 {
                formula(s, i, 0, &ptg_exp(0, 0), 8);
            }
        },
    )
}

fn supbook_with_a_huge_length_character() -> Vec<u8> {
    // External SUPBOOKs whose path is U+0005 then U+10FFFF.
    one_sheet(
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
    )
}

fn shared_3d_reference_to_another_sheet() -> Vec<u8> {
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    s.rec(0x01AE, &cat(&[&u16le(2), &u16le(0x0401)]));
    s.rec(0x0017, &cat(&[&u16le(1), &u16le(0), &u16le(1), &u16le(1)]));
    let b1 = boundsheet(&mut s, "Sheet1");
    let b2 = boundsheet(&mut s, "Sheet2");
    eof(&mut s);
    let start = s.pos();
    patch(&mut s, b1, start);
    bof8(&mut s, 0x0010);
    let ref3d = cat(&[&[0x5A], &u16le(0), &u16le(0), &u16le(0xC000)]);
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
    s.bytes
}

fn external_sheet_range() -> Vec<u8> {
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
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
    s.bytes
}

fn boundsheet_offset_at_u32_max() -> Vec<u8> {
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    let b = boundsheet(&mut s, "Far");
    eof(&mut s);
    s.bytes[b + 4..b + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    s.bytes
}

fn sst_ext_rst_of_u32_max_bytes() -> Vec<u8> {
    let sst = cat(&[
        &u32le(1),
        &u32le(1),
        &u16le(1),
        &[0x04],
        &u32le(u32::MAX),
        b"x",
    ]);
    one_sheet(
        |s| {
            s.rec(0x00FC, &sst);
        },
        |_| {},
    )
}

fn inverted_merged_range() -> Vec<u8> {
    one_sheet(
        |_| {},
        |s| {
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
    )
}

fn operands_that_add_up_on_the_stack() -> Vec<u8> {
    // A 255-character defined name used 13,000 times in one formula
    // without operators.
    let name = [b'N'; 255];
    let mut tokens = Vec::new();
    for _ in 0..13_000 {
        tokens.extend_from_slice(&[0x23, 1, 0, 0, 0]);
    }
    one_sheet(
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
    )
}

fn many_empty_records() -> Vec<u8> {
    // 1.2 million empty records of an unknown type in one sheet.
    one_sheet(
        |_| {},
        |s| {
            for _ in 0..1_200_000 {
                s.rec(0x7FFF, &[]);
            }
        },
    )
}

fn broken_filepass(body: &[u8]) -> Vec<u8> {
    one_sheet(
        |s| {
            s.rec(0x002F, body);
        },
        |_| {},
    )
}

#[test]
fn hostile_xls_streams() {
    let mut cases = vec![
        (
            "many BOUNDSHEETs on one substream",
            xls(many_boundsheets_on_one_substream()),
        ),
        (
            "a BOUNDSHEET into the globals",
            xls(boundsheet_into_the_globals()),
        ),
        (
            "an SST with thousands of CONTINUEs",
            xls(sst_with_thousands_of_continues()),
        ),
        (
            "40,000 missing shared formula anchors",
            xls(missing_shared_formula_anchors()),
        ),
        (
            "20,000 shared formulas at their anchors",
            xls(shared_formulas_each_at_its_anchor()),
        ),
        (
            "one 64 KB shared formula in 20,000 cells",
            xls(one_big_shared_formula_in_many_cells()),
        ),
        (
            "4,000 SUPBOOKs with a huge length character",
            xls(supbook_with_a_huge_length_character()),
        ),
        (
            "a shared 3D reference",
            xls(shared_3d_reference_to_another_sheet()),
        ),
        ("an external sheet range", xls(external_sheet_range())),
        (
            "a BOUNDSHEET offset of u32::MAX",
            xls(boundsheet_offset_at_u32_max()),
        ),
        (
            "an SST ExtRst of u32::MAX bytes",
            xls(sst_ext_rst_of_u32_max_bytes()),
        ),
        ("an inverted merged range", xls(inverted_merged_range())),
        (
            "13,000 long names on the formula stack",
            xls(operands_that_add_up_on_the_stack()),
        ),
        ("1.2 million empty records", xls(many_empty_records())),
        ("an empty FILEPASS", xls(broken_filepass(&[]))),
        (
            "an RC4 FILEPASS cut short",
            xls(broken_filepass(&[1, 0, 1, 0])),
        ),
    ];
    // A container that hides a stream named like the workbook, twice.
    let stream = one_sheet(|_| {}, |_| {});
    cases.push((
        "two Workbook streams",
        cfb::compound(&[("Workbook", &stream), ("WORKBOOK", &[0u8; 8])]),
    ));
    let cases = cases
        .into_iter()
        .map(|(case, bytes)| (case, "book.xls", bytes))
        .collect();
    run_all("hostile xls", cases).settle(WAIVERS);
}

#[test]
fn old_excel_versions_are_refused_clearly() {
    let cases: Vec<(&str, &'static str, Vec<u8>)> = vec![
        ("biff2 sheet, raw", "book.xls", biff2_sheet()),
        ("biff4 workbook, raw", "book.xls", biff4_workbook()),
        (
            "biff5 cp936 as Workbook",
            "book.xls",
            xls(biff5_book(936, b"\xB1\xED", b"\xD6\xD0\xCE\xC4")),
        ),
        (
            "biff5 cp1252 as Book",
            "book.xls",
            cfb::compound(&[("Book", &biff5_book(1252, b"Sheet", b"caf\xE9"))]),
        ),
    ];
    let mut report = Report::default();
    for (case, name, bytes) in cases {
        let outcome = report.run(case.to_owned(), Input::Bytes(name, bytes), Depth::Save);
        let Some(outcome) = outcome else { continue };
        println!("{case}: {}", describe(&outcome));
        match outcome.refused() {
            Some(e) if mentions(&e.why, &["BIFF", "Excel 5", "version"]) => {}
            _ => report.fail(case, format!("not refused with its version: {outcome:?}")),
        }
    }
    println!("{}", report.summary("old Excel"));
    report.settle(WAIVERS);
}

/// An .xls is a compound file. Anything else named .xls is refused, not
/// read as text: the bytes of an old or a bare BIFF stream are not a table.
#[test]
fn xls_that_is_not_a_compound_file_is_refused() {
    let cases: Vec<(&str, &'static str, Vec<u8>)> = vec![
        (
            "a raw BIFF8 stream without a container",
            "book.xls",
            one_sheet(|_| {}, |_| {}),
        ),
        (
            "text named .xls",
            "book.xls",
            b"Attachment: a,b\n1,2\n".to_vec(),
        ),
    ];
    let mut report = Report::default();
    for (case, name, bytes) in cases {
        let outcome = report.run(case.to_owned(), Input::Bytes(name, bytes), Depth::Save);
        let Some(outcome) = outcome else { continue };
        println!("{case}: {}", describe(&outcome));
        if outcome.refused().is_none() {
            report.fail(case, "opened as text".into());
        }
    }
    report.settle(WAIVERS);
}

#[test]
fn hostile_csv_text() {
    let mut huge_field = String::with_capacity(9 << 20);
    huge_field.push_str("id,blob\n1,");
    huge_field.extend(std::iter::repeat_n('9', 8 << 20));
    huge_field.push('\n');
    let mut huge_header = "x".repeat(1 << 20);
    huge_header.push_str(",b\n");
    for i in 0..10 {
        huge_header.push_str(&format!("t{i},{i}\n"));
    }
    let mut wide_line = "1;".repeat(1_000_000);
    wide_line.push_str("1\n");
    let values = [
        "1".repeat(2000),
        format!("1{}", ",000".repeat(300)),
        "1e99999".into(),
        "1e-99999".into(),
        "--5".into(),
        "(-5)".into(),
        "5%%".into(),
        "$$5".into(),
        "12:60".into(),
        "13:00 PM".into(),
        "2026-13-01".into(),
        "99999-01-01".into(),
        "2026年13月1日".into(),
        "14:30:05.1234567890".into(),
        "=1+1".into(),
        "@SUM(1)".into(),
    ];
    let hostile_values = values
        .iter()
        .map(|v| format!("\"{}\"\n", v.replace('"', "\"\"")))
        .collect::<String>();
    let mut many_rows = String::from("zip\n");
    for i in 0..100_000 {
        many_rows.push_str(&format!("{:05}\n", i % 100_000));
    }
    let mut quotes = String::new();
    for _ in 0..50_000 {
        quotes.push_str("\"a\"\"b,");
    }
    let controls: String = (0u8..0x20)
        .map(|b| format!("x{}y,\u{202E}z\u{200B},\u{E0041}\n", b as char))
        .collect();
    let cases: Vec<(&str, &'static str, Vec<u8>)> = vec![
        ("an 8 MB field", "data.csv", huge_field.into_bytes()),
        ("a 1 MB header name", "data.csv", huge_header.into_bytes()),
        (
            "a line of a million delimiters",
            "data.csv",
            wide_line.into_bytes(),
        ),
        (
            "values no parser should take",
            "data.csv",
            hostile_values.into_bytes(),
        ),
        (
            "100,000 rows with leading zeros",
            "data.csv",
            many_rows.into_bytes(),
        ),
        (
            "an unclosed quote over 50,000 fields",
            "data.csv",
            quotes.into_bytes(),
        ),
        (
            "control and invisible characters",
            "data.csv",
            controls.into_bytes(),
        ),
        ("a lone UTF-16 BOM", "data.csv", vec![0xFF, 0xFE]),
        (
            "an odd-length UTF-16 file",
            "data.csv",
            vec![0xFF, 0xFE, b'a', 0, b'b'],
        ),
        (
            "unpaired UTF-16 surrogates",
            "data.csv",
            vec![0xFF, 0xFE, 0x00, 0xD8, b',', 0, 0x00, 0xDC],
        ),
        (
            "invalid UTF-8 after a BOM",
            "data.csv",
            vec![0xEF, 0xBB, 0xBF, 0xC3, 0x28, b',', 0xFF],
        ),
        ("NUL bytes", "data.csv", b"a,\0b\n\0\0\0,c\n".to_vec()),
        ("only line breaks", "data.csv", vec![b'\n'; 200_000]),
        (
            "a sep= line naming a control character",
            "data.csv",
            b"sep=\x01\na\x01b\n".to_vec(),
        ),
    ];
    run_all("hostile csv", cases).settle(WAIVERS);
}

/// An agile EncryptionInfo around `xml`, with a package of 64 zero bytes.
fn agile(xml: &str) -> Vec<u8> {
    let mut info = vec![4, 0, 4, 0, 0x40, 0, 0, 0];
    info.extend_from_slice(xml.as_bytes());
    cfb::compound(&[("EncryptionInfo", &info), ("EncryptedPackage", &[0u8; 64])])
}

fn agile_key(spin: &str, params: &str) -> String {
    let b16 = "AAAAAAAAAAAAAAAAAAAAAA==";
    let b32 = "A".repeat(43) + "=";
    let b64 = "A".repeat(86) + "==";
    let key = format!(
        r#"spinCount="{spin}" {params} encryptedVerifierHashInput="{b16}" encryptedVerifierHashValue="{b64}" encryptedKeyValue="{b32}""#
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><encryption xmlns="http://schemas.microsoft.com/office/2006/encryption" xmlns:p="http://schemas.microsoft.com/office/2006/keyEncryptor/password"><keyData {params}/><keyEncryptors><keyEncryptor uri="http://schemas.microsoft.com/office/2006/keyEncryptor/password"><p:encryptedKey {key}/></keyEncryptor></keyEncryptors></encryption>"#
    )
}

const PARAMS: &str = r#"saltSize="16" blockSize="16" keyBits="256" hashSize="64" cipherAlgorithm="AES" cipherChaining="ChainingModeCBC" hashAlgorithm="SHA512" saltValue="AAAAAAAAAAAAAAAAAAAAAA==""#;

#[test]
fn hostile_encrypted_packages() {
    let huge_salt = PARAMS.replace("AAAAAAAAAAAAAAAAAAAAAA==", &"A".repeat(500_000));
    let deep = format!("{}{}", "<a>".repeat(100_000), "</a>".repeat(100_000));
    let mut header_only = vec![3, 0, 2, 0, 0x24, 0, 0, 0];
    header_only.extend(u32::MAX.to_le_bytes());
    let cases: Vec<(&str, &'static str, Vec<u8>)> = vec![
        (
            "a spin count of 4 billion",
            "book.xlsx",
            agile(&agile_key("4000000000", PARAMS)),
        ),
        (
            "key bits of 99,999",
            "book.xlsx",
            agile(&agile_key(
                "1",
                &PARAMS.replace(r#"keyBits="256""#, r#"keyBits="99999""#),
            )),
        ),
        (
            "an unknown cipher",
            "book.xlsx",
            agile(&agile_key(
                "1",
                &PARAMS.replace(r#"cipherAlgorithm="AES""#, r#"cipherAlgorithm="3DES""#),
            )),
        ),
        (
            "a 500 KB salt",
            "book.xlsx",
            agile(&agile_key("1", &huge_salt)),
        ),
        ("nested XML 100,000 deep", "book.xlsx", agile(&deep)),
        ("not XML", "book.xlsx", agile("\u{0}\u{1}not xml")),
        (
            "a standard header of u32::MAX bytes",
            "book.xlsx",
            cfb::compound(&[
                ("EncryptionInfo", &header_only),
                ("EncryptedPackage", &[0u8; 8]),
            ]),
        ),
        (
            "an EncryptedPackage without EncryptionInfo",
            "book.xls",
            cfb::compound(&[("EncryptedPackage", &[0xFFu8; 600])]),
        ),
    ];
    run_all("hostile encryption", cases).settle(WAIVERS);
}

#[test]
fn hostile_xlsb_packages() {
    use xlsb::{package, rec, row_header, sheet_part, strings_part, wide, workbook_part};
    let book = workbook_part();
    let strings = strings_part(&["a"]);
    let sheet_with = |records: &dyn Fn(&mut Vec<u8>)| {
        let mut data = Vec::new();
        records(&mut data);
        sheet_part(&data)
    };
    // A string that claims 2^31 characters.
    let huge_string = sheet_with(&|d| {
        rec(d, 0, &row_header(0));
        let mut cell = [0u32.to_le_bytes(), 0u32.to_le_bytes()].concat();
        cell.extend(0x7FFF_FFFFu32.to_le_bytes());
        rec(d, 6, &cell);
    });
    // A record whose size runs past the end of the part.
    let mut past_the_end = Vec::new();
    rec(&mut past_the_end, 129, &[]);
    past_the_end.extend([5, 0xFF, 0xFF, 0xFF, 0x7F]);
    // 64 and 65 nested future-record blocks.
    let nested = |depth: usize| {
        sheet_with(&|d| {
            for _ in 0..depth {
                rec(d, 35, &[0, 0, 0, 0]);
            }
        })
    };
    // Rows out of order, and a row past the last row.
    let unordered = sheet_with(&|d| {
        rec(d, 0, &row_header(5));
        rec(d, 0, &row_header(4));
    });
    let far_row = sheet_with(&|d| rec(d, 0, &row_header(u32::MAX)));
    // A shared-string part that claims a million strings and holds none.
    let mut claims = Vec::new();
    let million = 1_000_000u32.to_le_bytes();
    rec(&mut claims, 159, &[million, million].concat());
    rec(&mut claims, 160, &[]);
    // A shared-string index far past the end.
    let bad_index = sheet_with(&|d| {
        rec(d, 0, &row_header(0));
        let cell = [0u32, 0, u32::MAX].map(u32::to_le_bytes).concat();
        rec(d, 7, &cell);
    });
    // 100,000 rows of one number cell each: slow in a reader that is
    // quadratic in rows.
    let many_rows = sheet_with(&|d| {
        for r in 0..100_000u32 {
            rec(d, 0, &row_header(r));
            rec(d, 5, &xlsb::number_cell(0, f64::from(r)));
        }
    });
    // 2 million empty records of an unknown type.
    let many_records = sheet_with(&|d| {
        for _ in 0..2_000_000 {
            rec(d, 1000, &[]);
        }
    });
    // A sheet named by a 31-character name of unpaired surrogates.
    let mut bad_name_book = Vec::new();
    rec(&mut bad_name_book, 131, &[]);
    rec(&mut bad_name_book, 143, &[]);
    let mut sheet = vec![0, 0, 0, 0, 1, 0, 0, 0];
    sheet.extend(wide("rId1"));
    sheet.extend(31u32.to_le_bytes());
    sheet.extend(std::iter::repeat_n([0x00, 0xD8], 31).flatten());
    rec(&mut bad_name_book, 156, &sheet);
    rec(&mut bad_name_book, 144, &[]);
    rec(&mut bad_name_book, 132, &[]);
    let cases: Vec<(&str, &'static str, Vec<u8>)> = vec![
        (
            "an xlsb string of 2^31 characters",
            "book.xlsb",
            package(&book, &huge_string, None),
        ),
        (
            "an xlsb record past the end",
            "book.xlsb",
            package(&book, &past_the_end, None),
        ),
        (
            "64 nested xlsb blocks",
            "book.xlsb",
            package(&book, &nested(64), None),
        ),
        (
            "65 nested xlsb blocks",
            "book.xlsb",
            package(&book, &nested(65), None),
        ),
        (
            "xlsb rows out of order",
            "book.xlsb",
            package(&book, &unordered, None),
        ),
        (
            "an xlsb row past the last row",
            "book.xlsb",
            package(&book, &far_row, None),
        ),
        (
            "an xlsb SST that claims a million strings",
            "book.xlsb",
            package(&book, &sheet_part(&[]), Some(&claims)),
        ),
        (
            "an xlsb string index past the end",
            "book.xlsb",
            package(&book, &bad_index, Some(&strings)),
        ),
        (
            "100,000 xlsb rows",
            "book.xlsb",
            package(&book, &many_rows, None),
        ),
        (
            "2 million empty xlsb records",
            "book.xlsb",
            package(&book, &many_records, None),
        ),
        (
            "an xlsb sheet name of lone surrogates",
            "book.xlsb",
            package(&bad_name_book, &sheet_part(&[]), None),
        ),
        (
            "an xlsb package named .xlsx",
            "book.xlsx",
            xlsb::small_book(),
        ),
    ];
    let report = run_all("hostile xlsb", cases);
    report.settle(WAIVERS);
}

/// A protected file says so: the fix names the password, not damage.
#[test]
fn encryption_is_named() {
    let mut report = Report::default();
    let bytes = agile(&agile_key("100000", PARAMS));
    let outcome = report.run(
        "agile".into(),
        Input::Bytes("book.xlsx", bytes),
        Depth::Read,
    );
    if let Some(outcome) = outcome {
        match outcome.refused() {
            Some(e)
                if e.code == ErrorCode::UnsupportedFormat && mentions(&e.what, &["password"]) => {}
            _ => report.fail("agile", format!("not refused as protected: {outcome:?}")),
        }
    }
    report.settle(WAIVERS);
}
