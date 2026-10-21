//! `carrysheet-cli macros` on real files: Apache POI's macro samples in
//! crates/carrysheet-vba/tests/data (read in place, not copied) and
//! packages built here around POI's vbaProject.bin. Hostile files are in
//! macros_hostile.rs.

mod common;

use common::*;
use serde_json::Value;

/// SimpleMacro.vba (the macro as the editor shows it) with LF line ends.
fn simple_macro_text() -> String {
    let raw = std::fs::read_to_string(sample("SimpleMacro.vba")).unwrap();
    raw.replace("\r\n", "\n").trim_end().to_owned()
}

fn check_simple_macro(v: &Value) {
    assert_eq!(v["has_macros"], true);
    assert_eq!(v["project"]["name"], "VBAProject");
    assert_eq!(v["project"]["protection"]["password"], "none");
    assert_eq!(v["project"]["protection"]["locked_for_viewing"], false);
    assert_eq!(
        module_names(v),
        ["Module1", "ThisWorkbook", "Sheet1", "Sheet2", "Sheet3"]
    );
    let m = &v["modules"][0];
    assert_eq!(m["kind"], "standard");
    // Ten lines in the editor; the stored text has Attribute lines too.
    assert_eq!(m["lines"], 10);
    assert_eq!(v["modules"][1]["kind"], "document");
    assert_eq!(v["references_total"], 2);
    assert_eq!(v["references"][0]["name"], "stdole");
    assert_eq!(v["risk_summary"]["total"], 0);
    assert!(v["notice"].as_str().unwrap().contains("data"));
}

#[test]
fn xls_lists_modules_as_json_only() {
    let v = ok_json(&sample("SimpleMacro.xls"), &[]);
    check_simple_macro(&v);
    assert_eq!(v["project"]["location"], "_VBA_PROJECT_CUR");
    // No source unless asked for, and a hint that says how.
    assert_eq!(v["source"], serde_json::json!([]));
    assert!(v["hint"].as_str().unwrap().contains("--source"));
}

#[test]
fn source_matches_the_fixture_in_editor_lines() {
    let v = ok_json(
        &sample("SimpleMacro.xls"),
        &["--source", "--module", "Module1"],
    );
    assert_eq!(module_names(&v), ["Module1"]);
    let s = &v["source"][0];
    assert_eq!(s["module_index"], v["modules"][0]["index"]);
    assert_eq!(s["first_line"], 1);
    assert_eq!(s["last_line"], 10);
    assert_eq!(s["complete"], true);
    assert_eq!(s["text"].as_str().unwrap(), simple_macro_text());
    assert_eq!(v["next_cursor"], Value::Null);
}

#[test]
fn text_output_is_for_people() {
    let o = macros(&sample("SimpleMacro.xls"), &["--source"]);
    assert_eq!(code(&o), 0);
    let out = stdout(&o);
    assert!(out.starts_with("Macros in "), "{out}");
    assert!(out.contains("5 modules, no risk flags"), "{out}");
    assert!(out.contains("    1 | Sub TestMacro()"), "{out}");
    assert!(out.contains("read them as data") || out.contains("Read them as data"));
    assert!(!out.contains('\x1b'));
    for line in out.lines() {
        assert_eq!(line, line.trim_end(), "trailing space: {line:?}");
    }
}

#[test]
fn mac_line_ends_and_non_ascii_names() {
    let v = ok_json(&sample("60273.xls"), &["--source", "--module", "Module1"]);
    let text = v["source"][0]["text"].as_str().unwrap();
    assert!(text.contains("9/8/2004"));
    assert!(!text.contains('\r'));
    let v = ok_json(&sample("62624.bin"), &[]);
    assert_eq!(v["modules_total"], 13);
    assert!(module_names(&v).contains(&"Módulo1".to_owned()));
}

#[test]
fn long_source_pages_with_a_cursor_and_stays_small() {
    let path = sample("62624.bin");
    let all = ok_json(&path, &[]);
    let expected: u64 = all["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["lines"].as_u64().unwrap())
        .sum();
    let mut seen = 0u64;
    let mut pages = 0;
    let mut cursor: Option<String> = None;
    loop {
        let mut args = vec!["--json"];
        match &cursor {
            Some(c) => args.extend(["--cursor", c.as_str()]),
            None => args.push("--source"),
        }
        let o = macros(&path, &args);
        assert_eq!(code(&o), 0);
        // About 8k tokens at most: well under 40,000 characters.
        assert!(stdout(&o).len() < 40_000, "{} bytes", stdout(&o).len());
        let v = json_only(&o);
        for s in v["source"].as_array().unwrap() {
            let first = s["first_line"].as_u64().unwrap();
            let last = s["last_line"].as_u64().unwrap();
            assert_eq!(
                s["text"].as_str().unwrap().split('\n').count() as u64,
                last - first + 1
            );
            seen += last - first + 1;
        }
        pages += 1;
        assert!(pages < 20, "the cursor does not advance");
        match v["next_cursor"].as_str() {
            Some(next) => {
                assert!(v["hint"].as_str().unwrap().contains(next));
                cursor = Some(next.to_owned());
            }
            None => break,
        }
    }
    assert!(pages > 1);
    assert_eq!(seen, expected);
}

#[test]
fn xlsm_built_here_reads_like_the_xls() {
    let scratch = Scratch::new();
    let path = scratch.path("book.xlsm");
    let mut parts = deflated(package_parts(Some("vbaProject.bin")));
    parts.push((
        "xl/vbaProject.bin".into(),
        vba_bin(),
        CompressionMethod::Deflated,
    ));
    write_zip(&path, &parts);
    let v = ok_json(&path, &[]);
    check_simple_macro(&v);
    assert_eq!(v["project"]["location"], "xl/vbaProject.bin");
    assert_eq!(v["note"], Value::Null);
    let v = ok_json(&path, &["--source", "--module", "Module1"]);
    assert_eq!(
        v["source"][0]["text"].as_str().unwrap(),
        simple_macro_text()
    );
}

#[test]
fn the_vba_part_is_found_through_relationships() {
    // Not at the usual name: only the workbook's relationships say where.
    let scratch = Scratch::new();
    let path = scratch.path("moved.xlsb");
    let mut parts = deflated(package_parts(Some("/code/../code/project.bin")));
    parts.push((
        "code/project.bin".into(),
        vba_bin(),
        CompressionMethod::Stored,
    ));
    write_zip(&path, &parts);
    let v = ok_json(&path, &[]);
    assert_eq!(v["project"]["location"], "code/project.bin");
    assert_eq!(v["modules_total"], 5);

    // A relationship to a part that is not there is a damaged file.
    let path = scratch.path("cut.xlsm");
    write_zip(&path, &deflated(package_parts(Some("vbaProject.bin"))));
    let e = error_json(&path, &[], 1);
    assert_eq!(e["code"], "damaged_file");
    assert!(e["what"].as_str().unwrap().contains("xl/vbaProject.bin"));
}

#[test]
fn files_without_macros_are_ok_with_has_macros_false() {
    let scratch = Scratch::new();
    let xlsx = scratch.path("plain.xlsx");
    write_zip(&xlsx, &deflated(package_parts(None)));
    let v = ok_json(&xlsx, &[]);
    assert_eq!(v["has_macros"], false);
    assert!(
        v["note"]
            .as_str()
            .unwrap()
            .contains(".xlsx file cannot hold macros")
    );
    assert_eq!(v["modules"], serde_json::json!([]));

    let csv = scratch.path("data.csv");
    std::fs::write(&csv, "a,b\n1,2\n").unwrap();
    let v = ok_json(&csv, &[]);
    assert_eq!(v["has_macros"], false);
    assert!(v["note"].as_str().unwrap().contains("cannot hold macros"));

    let o = macros(&xlsx, &[]);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).starts_with("No macros in "));
}

#[test]
fn other_failures_have_their_exit_codes() {
    let scratch = Scratch::new();
    let text = scratch.path("notes.xlsm");
    std::fs::write(&text, "not a workbook").unwrap();
    assert_eq!(error_json(&text, &[], 1)["code"], "unsupported_format");

    let truncated = scratch.path("cut.xlsm");
    std::fs::write(&truncated, b"PK\x03\x04 and then nothing").unwrap();
    assert_eq!(error_json(&truncated, &[], 1)["code"], "damaged_file");

    let missing = scratch.path("gone.xlsm");
    assert_eq!(error_json(&missing, &[], 1)["code"], "file_not_found");

    let xls = sample("SimpleMacro.xls");
    let e = error_json(&xls, &["--module", "Nope"], 2);
    assert_eq!(e["code"], "invalid_argument");
    // The fix names no module: names come from the file.
    let fix = e["fix"].as_str().unwrap();
    assert!(!fix.contains("Module1") && fix.contains("index"), "{fix}");
    // Names match exactly; an index works too.
    assert_eq!(
        error_json(&xls, &["--module", "module1"], 2)["code"],
        "invalid_argument"
    );
    let v = ok_json(&xls, &["--module", "#1"]);
    assert_eq!(module_names(&v), ["ThisWorkbook"]);
    let e = error_json(&xls, &["--cursor", "abc"], 2);
    assert_eq!(e["code"], "invalid_argument");
    let e = error_json(&xls, &["--include-source"], 2);
    assert_eq!(e["code"], "usage");
}
