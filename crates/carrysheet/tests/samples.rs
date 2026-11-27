//! Apache POI samples with macros and charts: a save keeps them, or says
//! before saving that they would be lost (record 0003, point 3). Then the
//! csv samples in `tests/data/formats/csv`.

use std::path::{Path, PathBuf};

use carrysheet::commands::{COMMANDS, CommandError, ErrorCode, validate_args};
use carrysheet::{Kernel, run, run_within};
use serde_json::{Map, Value, json};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("carrysheet-samples-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn call(name: &str, args: Value) -> Result<Map<String, Value>, CommandError> {
    let command = COMMANDS.iter().find(|c| c.name == name).unwrap();
    let args = validate_args(command, &args)?;
    run(&Kernel::new(), command, &args)
}

fn p(path: &Path) -> String {
    path.display().to_string()
}

/// A saved package, opened with the hardened zip reader.
fn open_zip(path: &Path) -> carrysheet_zip::Archive<std::fs::File> {
    let file = std::fs::File::open(path).unwrap();
    carrysheet_zip::Archive::open(file, carrysheet_zip::Limits::default()).unwrap()
}

/// The part names of a saved package, lower case.
fn parts(path: &Path) -> Vec<String> {
    open_zip(path)
        .entries()
        .iter()
        .map(|e| e.name.to_ascii_lowercase())
        .collect()
}

fn part_bytes(path: &Path, name: &str) -> Vec<u8> {
    let mut zip = open_zip(path);
    let index = zip.find(name).unwrap();
    zip.read(index, u64::MAX).unwrap()
}

fn part_text(path: &Path, name: &str) -> String {
    String::from_utf8(part_bytes(path, name)).unwrap()
}

fn lost(done: &Map<String, Value>) -> Vec<String> {
    done["save"]["lost"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["feature"].as_str().unwrap().to_owned())
        .collect()
}

/// `overview` says a file has macros, that they are never run and that a
/// save keeps them; a package without macros gets no such field.
#[test]
fn overview_says_macros_are_kept_and_never_run() {
    let done = call("overview", json!({ "path": p(&data("SimpleMacro.xlsm")) })).unwrap();
    let macros = done["macros"].as_str().unwrap();
    assert!(macros.contains("VBA project"), "{macros}");
    assert!(
        macros.contains("never runs") && macros.contains("keeps them"),
        "{macros}"
    );

    let done = call(
        "overview",
        json!({ "path": p(&data("WithTwoCharts.xlsx")) }),
    )
    .unwrap();
    assert!(done.get("macros").is_none(), "{done:?}");
}

/// A stored zip of `parts`, for a package rewritten by a test.
fn stored_zip(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, body) in parts {
        let mut crc = flate2::Crc::new();
        crc.update(body);
        let (crc, size, at) = (crc.sum(), body.len() as u32, out.len() as u32);
        let fixed = |sig: u32| {
            let mut h = sig.to_le_bytes().to_vec();
            h.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&size.to_le_bytes());
            h.extend_from_slice(&size.to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0, 0]);
            h
        };
        out.extend(fixed(0x0403_4b50));
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(body);
        // The central header has "version made by" before the local fields.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 0]);
        central.extend_from_slice(&fixed(0)[4..]);
        central.extend_from_slice(&[0; 10]);
        central.extend_from_slice(&at.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let (start, len, n) = (out.len() as u32, central.len() as u32, parts.len() as u16);
    out.extend(central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// Excel finds the VBA project by relationship and content type, so a
/// hostile file can store it under any part name: `overview` still says so.
#[test]
fn overview_finds_a_renamed_vba_project() {
    let src = data("SimpleMacro.xlsm");
    let mut zip = open_zip(&src);
    let names: Vec<String> = zip.entries().iter().map(|e| e.name.clone()).collect();
    let mut rewritten = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let body = zip.read(i, u64::MAX).unwrap();
        let (name, body) = match name.as_str() {
            "xl/vbaProject.bin" => ("xl/project1.bin".to_owned(), body),
            "[Content_Types].xml" | "xl/_rels/workbook.xml.rels" => {
                let text = String::from_utf8(body)
                    .unwrap()
                    .replace("vbaProject.bin", "project1.bin");
                (name.clone(), text.into_bytes())
            }
            _ => (name.clone(), body),
        };
        rewritten.push((name, body));
    }
    let dir = scratch("renamed");
    let path = dir.join("renamed.xlsm");
    std::fs::write(&path, stored_zip(&rewritten)).unwrap();
    assert!(!parts(&path).contains(&"xl/vbaproject.bin".to_owned()));

    let done = call("overview", json!({ "path": p(&path) })).unwrap();
    let macros = done["macros"].as_str().unwrap_or_default();
    assert!(macros.contains("VBA project"), "{done:?}");
}

/// Carrysheet cannot see macros in an .xls file: `overview` says they were
/// not checked rather than staying silent.
#[test]
fn overview_says_macros_in_xls_were_not_checked() {
    let done = call(
        "overview",
        json!({ "path": p(&data("formats/biff/SimpleWithFormula.xls")) }),
    )
    .unwrap();
    let macros = done["macros"].as_str().unwrap();
    assert!(
        macros.starts_with("not checked:")
            && macros.contains(".xls")
            && macros.contains("never runs"),
        "{macros}"
    );
}

#[test]
fn macros_are_kept_in_xlsm_and_reported_before_xlsx() {
    let dir = scratch("macro");
    let src = data("SimpleMacro.xlsm");
    let ops = json!([{ "op": "set_value", "target": "B2", "value": "checked" }]);

    // Edited as .xlsm, the VBA project comes back byte for byte.
    let out = dir.join("macro.xlsm");
    let done = call(
        "edit",
        json!({ "path": p(&src), "ops": ops, "output": p(&out) }),
    )
    .unwrap();
    assert_eq!(done["written"], true, "{done:?}");
    assert!(lost(&done).is_empty(), "{done:?}");
    assert!(parts(&out).contains(&"xl/vbaproject.bin".to_owned()));
    let vba = part_bytes(&out, "xl/vbaProject.bin");
    let original = part_bytes(&src, "xl/vbaProject.bin");
    assert_eq!(vba, original);

    // As .xlsx the macros cannot stay: refused before writing, with the list.
    let out = dir.join("macro.xlsx");
    let e = call("convert", json!({ "path": p(&src), "output": p(&out) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfirmationRequired);
    assert!(e.what.contains("VBA project"), "{}", e.what);
    assert!(e.fix.contains(".xlsm"), "{}", e.fix);
    assert!(!out.exists());

    // dry_run shows the same report without writing.
    let dry = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "dry_run": true }),
    )
    .unwrap();
    assert_eq!(dry["written"], false);
    assert!(lost(&dry).contains(&"VBA project".to_owned()));

    // confirm (for overwriting) does not accept a loss: that has its own
    // confirm_loss, so one yes never covers both (review M1).
    let e = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "confirm": true }),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfirmationRequired);
    assert!(e.fix.contains("confirm_loss"), "{}", e.fix);
    assert!(!out.exists());

    // With confirm_loss it saves and says what was lost.
    let done = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "confirm_loss": true }),
    )
    .unwrap();
    assert_eq!(done["written"], true);
    assert!(lost(&done).contains(&"VBA project".to_owned()));
    assert!(!parts(&out).contains(&"xl/vbaproject.bin".to_owned()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn charts_are_kept_or_reported() {
    let dir = scratch("charts");
    let src = data("WithTwoCharts.xlsx");
    let before = call(
        "read",
        json!({ "path": p(&src), "range": "E1", "mode": "both" }),
    )
    .unwrap();
    assert_eq!(
        before["rows"][0]["cells"][0],
        json!({ "value": 25.0, "formula": "=D1*D1" })
    );

    let out = dir.join("charts.xlsx");
    let ops = json!([{ "op": "set_value", "target": "D1", "value": 7 }]);
    let result = call(
        "edit",
        json!({ "path": p(&src), "ops": ops, "output": p(&out) }),
    );
    let saved = match result {
        Ok(done) => done,
        // Refused: then it must name what would be lost, and write nothing.
        Err(e) => {
            assert_eq!(e.code, ErrorCode::ConfirmationRequired, "{e:?}");
            assert!(!out.exists());
            call(
                "edit",
                json!({ "path": p(&src), "ops": ops, "output": p(&out), "confirm_loss": true }),
            )
            .unwrap()
        }
    };
    assert_eq!(
        saved["recalculated"][0],
        json!({ "sheet": "Sheet1", "cell": "E1", "before": 25.0, "after": 49.0 })
    );
    let charts = parts(&out)
        .iter()
        .filter(|n| n.starts_with("xl/charts/chart") && n.ends_with(".xml"))
        .count();
    let lost = lost(&saved);
    assert!(
        charts == 2 || lost.contains(&"charts".to_owned()),
        "charts kept: {charts}; reported lost: {lost:?}"
    );
    // Each chart still points at its data.
    if charts == 2 {
        assert!(
            part_text(&out, "xl/charts/chart1.xml").contains("Sheet"),
            "chart lost its series"
        );
    }
    // The saved file reopens with the new values.
    let after = call("read", json!({ "path": p(&out), "range": "D1:E1" })).unwrap();
    assert_eq!(after["rows"][0]["cells"], json!([7.0, 49.0]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csv_output_lists_what_it_cannot_hold() {
    let dir = scratch("csv");
    let src = data("WithTwoCharts.xlsx");
    let out = dir.join("values.csv");
    let e = call("convert", json!({ "path": p(&src), "output": p(&out) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfirmationRequired);
    for word in ["formulas", "charts"] {
        assert!(e.what.contains(word), "{word}: {}", e.what);
    }
    let done = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "confirm_loss": true }),
    )
    .unwrap();
    assert_eq!(done["format"], "csv");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.starts_with("1,10,,5,25"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

// The csv front end (record 0010, decision 3): encodings are decoded, and
// a field is a number only when the number prints back as the same text.

/// Writes `bytes` as `<name>.csv` and reads the whole sheet.
fn read_csv(name: &str, bytes: &[u8]) -> Result<Vec<Vec<Value>>, CommandError> {
    let dir = scratch(&format!("csv-{name}"));
    let path = dir.join(format!("{name}.csv"));
    std::fs::write(&path, bytes).unwrap();
    let page = call("read", json!({ "path": p(&path) }));
    let _ = std::fs::remove_dir_all(&dir);
    Ok(page?["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["cells"].as_array().unwrap().clone())
        .collect())
}

#[test]
fn csv_fields_become_numbers_only_when_nothing_changes() {
    let rows = read_csv(
        "values",
        b"007,1E5,+1,12345678901234567890,SEPT1,2024-01-02,TRUE,inf,-0,-0.00,+1.50,007.50,.50,1.50E2\n\
          3,4.5,-2,0.1,1.0,12.50,-3.00,0.10,0.00, 5\n",
    )
    .unwrap();
    let text = [
        "007",
        "1E5",
        "+1",
        "12345678901234567890",
        "SEPT1",
        "2024-01-02",
        "TRUE",
        "inf",
        "-0",
        "-0.00",
        "+1.50",
        "007.50",
        ".50",
        "1.50E2",
    ];
    assert_eq!(rows[0], text.map(|t| json!(t)));
    let numbers = [3.0, 4.5, -2.0, 0.1, 1.0, 12.5, -3.0, 0.1, 0.0];
    assert_eq!(rows[1][..9], numbers.map(|n| json!(n)));
    assert_eq!(rows[1][9], json!(" 5"));
}

#[test]
fn csv_fields_past_excel_digits_stay_text() {
    // 30, 31 and 40 decimals, and a long run of zeros.
    let zeros = |n: usize| format!("0.5{}", "0".repeat(n - 1));
    let (d30, d31, d40) = (zeros(30), zeros(31), zeros(40));
    let long = format!("1.{}", "0".repeat(1_500));
    let line = format!("{d30},{d31},{d40},{long},4111111111111111,123456789012345,1.10\n");
    let rows = read_csv("digits", line.as_bytes()).unwrap();
    assert_eq!(rows[0][0], json!(0.5));
    let text = [d31.as_str(), &d40, &long, "4111111111111111"];
    assert_eq!(rows[0][1..5], text.map(|t| json!(t)));
    assert_eq!(rows[0][5..], [json!(123456789012345.0), json!(1.1)]);
}

#[test]
fn csv_decimals_with_trailing_zeros_sum_and_save_as_written() {
    let dir = scratch("csv-money");
    // The last row: 15 digits, 1E20 and 1E-20 written out, and a negative.
    let edges =
        "123456789012345.0,100000000000000000000.00,0.00000000000000000001000,-99999999999999.90";
    let text = format!("a,b,c,d\r\n12.50,-3.00,7,8\r\n0.10,1.0,9,10\r\n{edges}\r\n");
    let src = dir.join("money.csv");
    std::fs::write(&src, &text).unwrap();
    let rows = read_csv("edges", edges.as_bytes()).unwrap();
    let numbers = [123456789012345.0, 1e20, 1e-20, -99999999999999.9];
    assert_eq!(rows[0], numbers.map(|n| json!(n)));
    let sum = call("eval", json!({ "path": p(&src), "formula": "=SUM(A2:B3)" })).unwrap();
    let total = sum["value"].as_f64().unwrap();
    assert!((total - 10.6).abs() < 1e-9, "{sum:?}");
    // csv to csv and csv to xlsx to csv, with the default (escaping) export.
    let plain = dir.join("plain.csv");
    call("convert", json!({ "path": p(&src), "output": p(&plain) })).unwrap();
    assert_eq!(std::fs::read_to_string(&plain).unwrap(), text);
    let book = dir.join("money.xlsx");
    call("convert", json!({ "path": p(&src), "output": p(&book) })).unwrap();
    let back = dir.join("back.csv");
    let args = json!({ "path": p(&book), "output": p(&back), "confirm_loss": true });
    call("convert", args).unwrap();
    assert_eq!(std::fs::read_to_string(&back).unwrap(), text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csv_legacy_and_unicode_encodings_are_decoded() {
    let cases = [
        ("legacy/gbk.csv", "张伟", "这个问题已经解决了"),
        ("legacy/big5.csv", "陳大文", "我們的產品價格是多少"),
        ("legacy/shift_jis.csv", "山田太郎", "ありがとうございます"),
        ("legacy/euc-kr.csv", "김철수", "감사합니다"),
        ("legacy/gb18030.csv", "张伟", "\u{20000}"),
        ("legacy/windows-1252.csv", "René", "naïve façade"),
        ("unicode/utf16be-bom.csv", "Ada", "Zürich"),
        ("unicode/utf16le-bom-tab.txt", "Ada", "Zürich"),
        ("unicode/utf8-bom-semicolon.csv", "Ada", "Zürich"),
    ];
    for (file, first, last) in cases {
        let bytes = std::fs::read(data("formats/csv").join(file)).unwrap();
        let rows = read_csv("encoding", &bytes).unwrap();
        assert_eq!(rows[1][0], json!(first), "{file}: {rows:?}");
        assert!(rows.iter().flatten().any(|v| v == last), "{file}: {rows:?}");
        let fields: Vec<_> = rows.iter().flatten().filter_map(|v| v.as_str()).collect();
        assert!(fields.iter().all(|f| !f.contains('\u{FFFD}')), "{file}");
        // A guessed encoding is named in the overview's warnings.
        let dir = scratch("csv-warning");
        let path = dir.join("guess.csv");
        std::fs::write(&path, &bytes).unwrap();
        let overview = call("overview", json!({ "path": p(&path) })).unwrap();
        let warnings = overview.get("warnings").map(Value::to_string);
        let warnings = warnings.unwrap_or_default();
        assert_eq!(
            warnings.contains("by a guess"),
            file.starts_with("legacy/"),
            "{file}: {warnings}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn csv_legacy_bytes_after_a_long_ascii_start_are_guessed() {
    let mut bytes = "a,1\n".repeat(300_000).into_bytes();
    bytes.extend(encoding_rs::GBK.encode("姓名,城市\n").0.as_ref());
    let dir = scratch("csv-late");
    let path = dir.join("late.csv");
    std::fs::write(&path, &bytes).unwrap();
    let args = json!({ "path": p(&path), "range": "A300001:B300001" });
    let page = call("read", args).unwrap();
    assert_eq!(
        page["rows"][0]["cells"],
        json!(["姓名", "城市"]),
        "{page:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csv_short_legacy_files_report_what_chardetng_guesses() {
    use encoding_rs::{BIG5, GBK, SHIFT_JIS, WINDOWS_1252};
    // Detection is weak on a few bytes. This pins what chardetng 0.1.17
    // gives: `false` marks a wrong guess, read as text in another encoding
    // (one GBK character, one or two Shift_JIS kanji, two katakana).
    let cases = [
        (GBK, "张", false),
        (GBK, "北京", true),
        (GBK, "张伟", true),
        (GBK, "姓名,城市", true),
        (BIG5, "臺", true),
        (BIG5, "臺北", true),
        (BIG5, "陳大文", true),
        (BIG5, "姓名,城市", true),
        (SHIFT_JIS, "東", false),
        (SHIFT_JIS, "東京", false),
        (SHIFT_JIS, "山田", false),
        (SHIFT_JIS, "名前,都市", true),
        (SHIFT_JIS, "メモ", false),
        (WINDOWS_1252, "café", true),
        (WINDOWS_1252, "señor,año", true),
        (WINDOWS_1252, "São Paulo", true),
        (WINDOWS_1252, "René,Zoë", true),
    ];
    let mut wrong = Vec::new();
    for (encoding, text, is_right) in cases {
        let (bytes, _, _) = encoding.encode(text);
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(&bytes, true);
        let guess = detector.guess(None, true).name();
        let opened = read_csv("short", &bytes);
        let first = match &opened {
            Ok(rows) => rows[0][0].to_string(),
            Err(e) => format!("error: {} / {} / {}", e.what, e.why, e.fix),
        };
        eprintln!(
            "short {}: {text:?} -> guess {guess}, A1 {first}",
            encoding.name()
        );
        let field = text.split(',').next().unwrap();
        let read_right = first == format!("{field:?}");
        if (guess == encoding.name()) != is_right || read_right != is_right {
            wrong.push(format!("{text}: guess {guess}, A1 {first}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn csv_bytes_that_fit_no_encoding_are_an_error() {
    // Valid UTF-8 for the whole guess sample, then a byte UTF-8 never has.
    let mut bytes = "Zürich,1\n".repeat(120_000).into_bytes();
    bytes.extend(b"\xFF,2\n");
    let e = read_csv("bad-utf8", &bytes).unwrap_err();
    assert_eq!(e.code, ErrorCode::DamagedFile, "{e:?}");
    assert!(
        e.why.contains("UTF-8") && e.why.contains("guessed"),
        "{}",
        e.why
    );
    assert!(e.fix.contains("UTF-8"), "{}", e.fix);
    // A byte order mark decides, so its encoding must hold every byte.
    let e = read_csv("bad-bom", b"\xEF\xBB\xBFa,\xFF\n").unwrap_err();
    assert_eq!(e.code, ErrorCode::DamagedFile, "{e:?}");
    assert!(e.why.contains("byte order mark"), "{}", e.why);
    let e = read_csv("bad-utf16", b"\xFF\xFEa\x00\x00\xD8,\x00").unwrap_err();
    assert!(e.why.contains("UTF-16LE"), "{}", e.why);
}

#[test]
fn csv_to_xlsx_to_csv_keeps_every_field() {
    let dir = scratch("csv-trip");
    let text = "id,code,amount,gene,note\r\n1,007,1E5,SEPT1,\"a, b\"\r\n2,+1,-2.5,MARCH1,12345678901234567890\r\n4,12.50,-3.00,0.10,1.0\r\n3,0.1,2024-01-02,TRUE,\"line one\nline two\"\r\n";
    let src = dir.join("trip.csv");
    std::fs::write(&src, text).unwrap();
    let book = dir.join("trip.xlsx");
    call("convert", json!({ "path": p(&src), "output": p(&book) })).unwrap();
    let out = dir.join("back.csv");
    let args =
        json!({ "path": p(&book), "output": p(&out), "confirm_loss": true, "csv_raw": true });
    call("convert", args).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csv_open_keeps_the_time_limit() {
    let dir = scratch("csv-time");
    let path = dir.join("slow.csv");
    std::fs::write(&path, "a,1\n".repeat(10_000)).unwrap();
    let command = COMMANDS.iter().find(|c| c.name == "overview").unwrap();
    let args = validate_args(command, &json!({ "path": p(&path) })).unwrap();
    let kernel = Kernel::new();
    let e = run_within(&kernel, command, &args, Some(0.0)).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded, "{e:?}");
    assert!(e.what.contains("took longer"), "{}", e.what);
    run_within(&kernel, command, &args, None).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
