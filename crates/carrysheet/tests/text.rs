//! Text from a file reaches a JSON result escaped (issue 67): tag
//! characters, bidi controls, terminal escapes and backslashes.

mod common;

use common::{Book, NS, call, p, scratch};

/// Tag characters spell "AB"; then a direction override, a colour escape
/// and a backslash that could fake an escape.
const HOSTILE: &str = "x\u{E0041}\u{E0042}\u{202E}\u{1b}[31m\\u{41}";
const ESCAPED: &str = "x\\u{e0041}\\u{e0042}\\u{202e}\\u{1b}[31m\\\\u{41}";

#[test]
fn a_hostile_sheet_name_is_escaped_in_json() {
    let dir = scratch("text-json");
    let path = dir.join("hostile.xlsx");
    let mut book = Book::new(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>a</t></is></c></row>"#);
    book.workbook = format!(
        r#"<?xml version="1.0"?><workbook {NS}><sheets><sheet name="{HOSTILE}" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    std::fs::write(&path, book.bytes()).unwrap();
    let done = call("overview", serde_json::json!({ "path": p(&path) })).unwrap();
    let name = done["sheets"][0]["name"].as_str().unwrap();
    assert_eq!(name, ESCAPED);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plain_cjk_and_emoji_sheet_names_are_kept() {
    let dir = scratch("text-plain");
    let path = dir.join("plain.xlsx");
    let mut book = Book::new(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>a</t></is></c></row>"#);
    book.workbook = format!(
        r#"<?xml version="1.0"?><workbook {NS}><sheets><sheet name="Módulo 日本語 😀" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    std::fs::write(&path, book.bytes()).unwrap();
    let done = call("overview", serde_json::json!({ "path": p(&path) })).unwrap();
    assert_eq!(done["sheets"][0]["name"], "Módulo 日本語 😀");
    let _ = std::fs::remove_dir_all(&dir);
}
