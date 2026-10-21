//! Hostile workbooks from review round 1 of issue 45 (H2, H3, M3, M7, L1):
//! each must fail fast with a three-part error or open correctly, never
//! hang or run out of memory (record 0003). The files are built here.

mod common;

use std::time::{Duration, Instant};

use carrysheet_commands::ErrorCode;
use common::*;
use serde_json::json;

/// Generous: a hang shows as minutes, and debug builds are slow.
const QUICK: Duration = Duration::from_secs(30);

fn open_error(name: &str, bytes: Vec<u8>) -> carrysheet_commands::CommandError {
    let dir = scratch(name);
    let path = dir.join(format!("{name}.xlsx"));
    std::fs::write(&path, bytes).unwrap();
    let e = call("overview", json!({ "path": p(&path) })).unwrap_err();
    assert!(
        !e.what.is_empty() && !e.why.is_empty() && !e.fix.is_empty(),
        "{e:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    e
}

#[test]
fn empty_deflate_blocks_stop_early() {
    // About 2.5 MB of empty compressed blocks that unpack to a tiny sheet:
    // before, every block was inflated (64 s for 33 MB).
    let data = sheet_xml(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#, "", "");
    let mut book = Book::with_sheet(&data);
    book.sheet_deflated = Some(empty_blocks(2_000_000, data.as_bytes()));
    let started = Instant::now();
    let e = open_error("empty-blocks", book.bytes());
    assert!(started.elapsed() < QUICK, "{:?}", started.elapsed());
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.why.contains("packed bytes"), "{}", e.why);
}

#[test]
fn many_full_width_columns_are_cheap() {
    // 100,000 entries for all 16,384 columns: each column is set once.
    let cols = r#"<col min="1" max="16384" width="20" customWidth="1"/>"#.repeat(100_000);
    let sheet = sheet_xml(
        r#"<row r="1"><c r="A1"><v>1</v></c></row>"#,
        &format!("<cols>{cols}</cols>"),
        "",
    );
    let dir = scratch("cols");
    let path = dir.join("cols.xlsx");
    std::fs::write(&path, Book::with_sheet(&sheet).bytes()).unwrap();
    let started = Instant::now();
    let o = call("overview", json!({ "path": p(&path) })).unwrap();
    assert!(started.elapsed() < QUICK, "{:?}", started.elapsed());
    assert_eq!(o["sheets"][0]["cells"], 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn one_long_shared_text_in_many_cells() {
    // 2 MB of text in 20,000 cells: 40 GB if copied per cell. The text is
    // shared, reading cuts it, and saving hashes it once.
    let text = "x".repeat(2 << 20);
    let mut rows = String::new();
    for r in 1..=200 {
        rows.push_str(&format!(r#"<row r="{r}">"#));
        for _ in 0..100 {
            rows.push_str(r#"<c t="s"><v>0</v></c>"#);
        }
        rows.push_str("</row>");
    }
    let mut book = Book::new(&rows);
    book.strings = Some(format!(
        r#"<?xml version="1.0"?><sst {NS}><si><t>{text}</t></si></sst>"#
    ));
    let dir = scratch("big-string");
    let path = dir.join("big.xlsx");
    std::fs::write(&path, book.bytes()).unwrap();
    let started = Instant::now();
    let o = call("overview", json!({ "path": p(&path) })).unwrap();
    assert_eq!(o["sheets"][0]["cells"], 20_000);
    let page = call("read", json!({ "path": p(&path), "range": "A1:B2" })).unwrap();
    let first = page["rows"][0]["cells"][0].as_str().unwrap();
    assert!(first.chars().count() <= 2_000, "{}", first.len());
    let cut = &page["cut"];
    assert_eq!(cut["total"], 4, "{cut}");
    assert!(cut["hint"].as_str().unwrap().contains("MID("), "{cut}");
    let out = dir.join("out.xlsx");
    call("convert", json!({ "path": p(&path), "output": p(&out) })).unwrap();
    assert!(started.elapsed() < QUICK, "{:?}", started.elapsed());
    // The saved file holds the text once.
    assert!(std::fs::metadata(&out).unwrap().len() < 5 << 20);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn too_many_styles_or_strings_are_refused() {
    let dir = scratch("counts");
    let path = dir.join("xf.xlsx");
    let mut book = Book::new(r#"<row r="1"><c r="A1" s="1"><v>1</v></c></row>"#);
    book.styles = Some(format!(
        r#"<?xml version="1.0"?><styleSheet {NS}><cellXfs>{}</cellXfs></styleSheet>"#,
        "<xf/>".repeat(200)
    ));
    std::fs::write(&path, book.bytes()).unwrap();
    let kernel = lowered(|l| l.styles = 100);
    let e = call_with(&kernel, "overview", json!({ "path": p(&path) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.why.contains("cellXfs"), "{}", e.why);
    // The default limit is Excel's.
    assert!(call("overview", json!({ "path": p(&path) })).is_ok());

    let path = dir.join("si.xlsx");
    let mut book = Book::new(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#);
    book.strings = Some(format!(
        r#"<?xml version="1.0"?><sst {NS}>{}</sst>"#,
        "<si/>".repeat(200)
    ));
    std::fs::write(&path, book.bytes()).unwrap();
    let kernel = lowered(|l| l.shared_strings = 100);
    let e = call_with(&kernel, "overview", json!({ "path": p(&path) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.why.contains("shared strings"), "{}", e.why);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn huge_array_results_are_errors_not_aborts() {
    // Two results of 10 million cells: before, the process aborted.
    let dir = scratch("sequence");
    let path = dir.join("seq.xlsx");
    let rows = r#"<row r="1"><c r="A1"><f>SEQUENCE(1000000,10)</f></c><c r="L1"><f>SEQUENCE(1000000,10)</f></c></row>"#;
    std::fs::write(&path, Book::new(rows).bytes()).unwrap();
    let started = Instant::now();
    let page = call("read", json!({ "path": p(&path), "range": "A1" })).unwrap();
    assert_eq!(page["rows"][0]["cells"][0], "#NUM!", "{page:?}");
    let e = call(
        "eval",
        json!({ "path": p(&path), "formula": "ROWS(SEQUENCE(1000000,10))" }),
    )
    .unwrap();
    assert_eq!(e["value"], "#NUM!", "{e:?}");
    assert!(started.elapsed() < QUICK, "{:?}", started.elapsed());
    let _ = std::fs::remove_dir_all(&dir);
}

fn numbers(n: u32) -> String {
    (1..=n)
        .map(|r| format!(r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c></row>"#))
        .collect()
}

#[test]
fn calculation_has_a_work_and_a_time_limit() {
    // Each of 2,000 formulas reads 4,000 cells: 8 million units.
    let dir = scratch("work");
    let path = dir.join("num.xlsx");
    std::fs::write(&path, Book::new(&numbers(2_000)).bytes()).unwrap();
    let ops = json!([{ "op": "set_formula", "target": "B1:B2000", "value": "SUMPRODUCT($A$1:$A$2000,$A$1:$A$2000)" }]);
    let out = dir.join("out.xlsx");
    let args = json!({ "path": p(&path), "ops": ops, "output": p(&out) });

    let kernel = lowered(|l| l.calc_work = 1_000_000);
    let e = call_with(&kernel, "edit", args.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.what.contains("more calculation"), "{}", e.what);
    assert!(e.fix.contains("nothing was written"), "{}", e.fix);
    assert!(!out.exists());

    let kernel = lowered(|l| l.calc_seconds = 0.0);
    let e = call_with(&kernel, "edit", args.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.what.contains("took longer"), "{}", e.what);
    assert!(!out.exists());

    // Within the limits the same edit works.
    call("edit", args).unwrap();
    let page = call("read", json!({ "path": p(&out), "range": "B1" })).unwrap();
    assert_eq!(page["rows"][0]["cells"][0], json!(2_668_667_000.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deep_chains_are_computed() {
    // A1 = A2+1, ..., A5000 = 1: deeper than the evaluator's recursion.
    let n = 5_000;
    let mut rows: String = (1..n)
        .map(|r| format!(r#"<row r="{r}"><c r="A{r}"><f>A{}+1</f></c></row>"#, r + 1))
        .collect();
    rows.push_str(&format!(r#"<row r="{n}"><c r="A{n}"><v>1</v></c></row>"#));
    let dir = scratch("chain");
    let path = dir.join("chain.xlsx");
    std::fs::write(&path, Book::new(&rows).bytes()).unwrap();
    let page = call("read", json!({ "path": p(&path), "range": "A1" })).unwrap();
    assert_eq!(page["rows"][0]["cells"][0], json!(5000.0));

    // The same chain made by an edit, in the other direction.
    let out = dir.join("out.xlsx");
    let ops = json!([
        { "op": "set_value", "target": "B5000", "value": 1 },
        { "op": "set_formula", "target": "B1:B4999", "value": "B2+1" }
    ]);
    call(
        "edit",
        json!({ "path": p(&path), "ops": ops, "output": p(&out) }),
    )
    .unwrap();
    let page = call("read", json!({ "path": p(&out), "range": "B1" })).unwrap();
    assert_eq!(page["rows"][0]["cells"][0], json!(5000.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn circular_references_are_named() {
    let dir = scratch("circ");
    let path = dir.join("circ.xlsx");
    let rows = r#"<row r="1"><c r="A1"><f>B1+1</f></c><c r="B1"><f>A1+1</f></c></row>"#;
    std::fs::write(&path, Book::new(rows).bytes()).unwrap();
    let page = call(
        "read",
        json!({ "path": p(&path), "range": "A1", "response_format": "detailed" }),
    )
    .unwrap();
    let cell = &page["rows"][0]["cells"][0];
    assert_eq!(cell["value"], "#CIRC!", "{cell}");
    let text = cell.to_string();
    assert!(!text.contains("#UNKNOWN!"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn damaged_xml_is_an_error() {
    let cell = r#"<row r="1"><c r="A1"><v>1</v></c></row>"#;
    let full = sheet_xml(cell, "", "");
    for (case, sheet) in [
        ("truncated", full[..full.len() - 20].to_owned()),
        ("mismatched", full.replace("</sheetData>", "</sheetDta>")),
        ("second-root", format!("{full}<worksheet/>")),
    ] {
        let e = open_error(case, Book::with_sheet(&sheet).bytes());
        assert_eq!(e.code, ErrorCode::DamagedFile, "{case}: {e:?}");
    }
}

#[test]
fn unreadable_numbers_are_reported() {
    let dir = scratch("nan");
    let path = dir.join("nan.xlsx");
    let rows = r#"<row r="1"><c r="A1"><v>NaN</v></c><c r="B1"><v>inf</v></c><c r="C1"><v>1e400</v></c><c r="D1"><v>2</v></c></row>"#;
    std::fs::write(&path, Book::new(rows).bytes()).unwrap();
    let o = call("overview", json!({ "path": p(&path) })).unwrap();
    let warnings = o["warnings"].to_string();
    assert!(
        warnings.contains("numeric cells could not be read"),
        "{o:?}"
    );
    let out = dir.join("out.xlsx");
    let dry = call(
        "convert",
        json!({ "path": p(&path), "output": p(&out), "dry_run": true }),
    )
    .unwrap();
    assert!(
        lost(&dry).iter().any(|l| l.contains("numeric cells")),
        "{dry:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_save_check_does_not_unpack_empty_blocks_either() {
    // A sheet-named part no relationship names: GridCraft never reads it,
    // but the save report's check looks into every sheet part.
    let mut book = Book::new(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
    let orphan = sheet_xml("", "", "");
    book.extra = vec![Entry {
        name: "xl/worksheets/sheet9.xml".into(),
        deflated: Some(empty_blocks(2_000_000, orphan.as_bytes())),
        data: orphan.into_bytes(),
    }];
    let dir = scratch("orphan");
    let path = dir.join("orphan.xlsx");
    std::fs::write(&path, book.bytes()).unwrap();
    let out = dir.join("out.xlsx");
    let started = Instant::now();
    let done = call(
        "convert",
        json!({ "path": p(&path), "output": p(&out), "dry_run": true }),
    )
    .unwrap();
    assert!(started.elapsed() < QUICK, "{:?}", started.elapsed());
    assert!(
        done["save"]["dropped_parts"].to_string().contains("sheet9"),
        "{done:?}"
    );
    // The call finished, so the clock did not stop it: the part was refused
    // by the unpack check (it reads far more than it gives), and the report
    // says what that check could not search.
    assert!(
        done["save"]["lost"].to_string().contains("not checked"),
        "{done:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lambda_loops_and_built_arrays_count_work() {
    // Round 2, N2: REDUCE over a million items, each building an array,
    // counted no work and ran without a time limit.
    let dir = scratch("lambda");
    let path = dir.join("lambda.xlsx");
    std::fs::write(&path, Book::new(&numbers(2)).bytes()).unwrap();
    let out = dir.join("out.xlsx");
    let args = json!({
        "path": p(&path),
        "ops": [{ "op": "set_formula", "target": "B1",
                  "value": "REDUCE(0,SEQUENCE(1000000),LAMBDA(a,b,a+SUM(SEQUENCE(10000))))" }],
        "output": p(&out),
    });
    let started = Instant::now();
    let e = call("edit", args.clone()).unwrap_err();
    assert!(started.elapsed() < QUICK, "{:?}", started.elapsed());
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(!out.exists());

    // The work count alone stops it too, long before the clock.
    let kernel = lowered(|l| l.calc_work = 1_000_000);
    let e = call_with(&kernel, "edit", args).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.what.contains("more calculation"), "{}", e.what);

    // MAP, SCAN, BYROW and MAKEARRAY call their lambda per item as well.
    for (i, formula) in [
        "SUM(MAP(SEQUENCE(1000),LAMBDA(x,SUM(SEQUENCE(1000)))))",
        "SUM(SCAN(0,SEQUENCE(1000),LAMBDA(a,b,a+SUM(SEQUENCE(1000)))))",
        "SUM(BYROW(SEQUENCE(1000),LAMBDA(r,SUM(SEQUENCE(1000)))))",
        "SUM(MAKEARRAY(1000,1,LAMBDA(r,c,SUM(SEQUENCE(1000)))))",
    ]
    .into_iter()
    .enumerate()
    {
        // One output file per formula: a file left by the one before would
        // make the next edit fail with FileExists instead.
        let out = dir.join(format!("out-{i}.xlsx"));
        let kernel = lowered(|l| l.calc_work = 100_000);
        let args = json!({
            "path": p(&path),
            "ops": [{ "op": "set_formula", "target": "B1", "value": formula }],
            "output": p(&out),
        });
        let e = call_with(&kernel, "edit", args.clone()).unwrap_err();
        assert_eq!(e.code, ErrorCode::LimitExceeded, "{formula}: {e:?}");
        assert!(!out.exists(), "{formula}");
        // Within the default limits each one is computed.
        call("edit", args).unwrap();
        assert!(out.exists(), "{formula}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nodes_inside_rows_and_strings_are_counted() {
    // Round 2, N3: elements inside a split element (`row`, `si`) were not
    // counted against the node limit.
    let dir = scratch("nodes");
    let junk = "<x a=\"1\"/>".repeat(5_000);
    let path = dir.join("row.xlsx");
    let row = format!(r#"<row r="1"><c r="A1"><v>1</v></c>{junk}</row>"#);
    std::fs::write(&path, Book::new(&row).bytes()).unwrap();
    let kernel = lowered(|l| l.xml_nodes = 2_000);
    let e = call_with(&kernel, "overview", json!({ "path": p(&path) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    // The count starts again for each row: many small rows are fine.
    let rows: String = (1..=2_000)
        .map(|r| format!(r#"<row r="{r}"><c r="A{r}"><v>1</v></c></row>"#))
        .collect();
    let small = dir.join("rows.xlsx");
    std::fs::write(&small, Book::new(&rows).bytes()).unwrap();
    let o = call_with(&kernel, "overview", json!({ "path": p(&small) })).unwrap();
    assert_eq!(o["sheets"][0]["cells"], 2_000);
    assert!(call("overview", json!({ "path": p(&path) })).is_ok());

    let path = dir.join("si.xlsx");
    let mut book = Book::new(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#);
    book.strings = Some(format!(
        r#"<?xml version="1.0"?><sst {NS}><si><t>a</t>{junk}</si></sst>"#
    ));
    std::fs::write(&path, book.bytes()).unwrap();
    let e = call_with(&kernel, "overview", json!({ "path": p(&path) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(call("overview", json!({ "path": p(&path) })).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Raw deflate of `data` with a flush after every byte: one literal per
/// block, a little under the inflate ratio (round 2, N6).
fn flushed_per_byte(data: &[u8]) -> Vec<u8> {
    use flate2::{Compress, Compression, FlushCompress, Status};
    let mut c = Compress::new(Compression::fast(), false);
    let mut out = Vec::new();
    for b in data {
        out.reserve(64);
        c.compress_vec(std::slice::from_ref(b), &mut out, FlushCompress::Partial)
            .unwrap();
    }
    assert_eq!(c.total_in(), data.len() as u64);
    loop {
        out.reserve(64);
        if c.compress_vec(&[], &mut out, FlushCompress::Finish)
            .unwrap()
            == Status::StreamEnd
        {
            break;
        }
    }
    out
}

#[test]
fn slow_unpacking_counts_against_the_time_limit() {
    let rows: String = (1..=4_000)
        .map(|r| format!(r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c></row>"#))
        .collect();
    let data = sheet_xml(&rows, "", "");
    let packed = flushed_per_byte(data.as_bytes());
    // Under the ratio: the ratio check alone lets it through.
    let ratio = u64::from(carrysheet_commands::limits::MIN_INFLATE_RATIO);
    assert!((packed.len() as u64) < data.len() as u64 * ratio);
    assert!(packed.len() > data.len() * 2, "{}", packed.len());
    let mut book = Book::with_sheet(&data);
    book.sheet_deflated = Some(packed);
    let dir = scratch("slow-inflate");
    let path = dir.join("slow.xlsx");
    std::fs::write(&path, book.bytes()).unwrap();

    let o = call("overview", json!({ "path": p(&path) })).unwrap();
    assert_eq!(o["sheets"][0]["cells"], 4_000);
    // Opening counts against the time limit (N5), unpacking included.
    let kernel = lowered(|l| l.calc_seconds = 0.0);
    let e = call_with(&kernel, "overview", json!({ "path": p(&path) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.what.contains("took longer"), "{}", e.what);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn one_time_budget_covers_the_whole_command() {
    use carrysheet_commands::engine::run::run_within;
    use carrysheet_commands::{COMMANDS, validate_args};
    // Round 2, N5: opening alone, with no calculation, counts as well.
    let dir = scratch("budget");
    let path = dir.join("num.xlsx");
    std::fs::write(&path, Book::new(&numbers(10)).bytes()).unwrap();
    let kernel = carrysheet_kernel::Kernel::new();
    let command = COMMANDS.iter().find(|c| c.name == "overview").unwrap();
    let args = validate_args(command, &json!({ "path": p(&path) })).unwrap();
    let e = run_within(&kernel, command, &args, Some(0.0)).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.why.contains("opening"), "{}", e.why);
    assert!(e.fix.contains("--max-seconds"), "{}", e.fix);
    // The budget ends with the command: the next one starts afresh.
    run_within(&kernel, command, &args, None).unwrap();
    run_within(&kernel, command, &args, Some(60.0)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
