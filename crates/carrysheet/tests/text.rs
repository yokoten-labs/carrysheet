//! Text from a file reaches a JSON result or an error escaped (issue 67):
//! tag characters, bidi controls and terminal escapes.

mod common;

use common::{Book, NS, call, p, scratch};
use serde_json::{Map, Value, json};

/// The overview of a one-sheet workbook whose sheet is named `name`.
fn overview_of(test: &str, name: &str) -> Result<Map<String, Value>, String> {
    let dir = scratch(test);
    let path = dir.join("book.xlsx");
    let mut book = Book::new(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>a</t></is></c></row>"#);
    book.workbook = format!(
        r#"<?xml version="1.0"?><workbook {NS}><sheets><sheet name="{name}" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    std::fs::write(&path, book.bytes()).unwrap();
    let done = call("overview", json!({ "path": p(&path) })).map_err(|e| e.what);
    let _ = std::fs::remove_dir_all(&dir);
    done
}

#[test]
fn hidden_characters_in_a_sheet_name_are_escaped_in_json() {
    // Tag characters spell "AB", then a direction override.
    let done = overview_of("text-json", "x\u{E0041}\u{E0042}\u{202E}y").unwrap();
    let want = "x\\u{e0041}\\u{e0042}\\u{202e}y";
    assert_eq!(done["sheets"][0]["name"], want);
}

#[test]
fn a_terminal_escape_in_a_sheet_name_is_escaped() {
    let done = overview_of("text-error", "x\u{1b}[31m\u{E0041}\u{202E}").unwrap();
    let what = done["sheets"][0]["name"].as_str().unwrap();
    assert!(!what.contains('\u{1b}') && !what.contains('\u{E0041}'), "{what:?}");
    assert!(what.contains("\\u{1b}") && what.contains("\\u{e0041}\\u{202e}"), "{what}");
}

#[test]
fn plain_cjk_and_emoji_sheet_names_are_kept() {
    let done = overview_of("text-plain", "Módulo 日本語 😀").unwrap();
    assert_eq!(done["sheets"][0]["name"], "Módulo 日本語 😀");
}
