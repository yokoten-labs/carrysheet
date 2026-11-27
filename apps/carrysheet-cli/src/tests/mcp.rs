use crate::files::{Root, home_dir};
use crate::mcp::*;
use carrysheet::args::{mcp_tools, tool_definition};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
#[path = "../../../../crates/carrysheet/tests/lint/mod.rs"]
mod lint;

use lint::lint_tools;
use serde_json::json;

/// The server for a root folder.
fn test_server(root: &Path, read_only: bool) -> Server {
    Server::new(Root::new(root).unwrap(), read_only)
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("carrysheet-mcp-unit-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("root")).unwrap();
    std::fs::write(dir.join("root/book.xlsx"), b"x").unwrap();
    std::fs::write(dir.join("outside.xlsx"), b"x").unwrap();
    dir
}

fn tools_json(server: &Server) -> Vec<Value> {
    server.tools().iter().map(|t| serde_json::to_value(t).unwrap()).collect()
}

#[test]
fn tools_match_the_table_exactly() {
    let dir = scratch("tools");
    let server = test_server(&dir.join("root"), false);
    let expected: Vec<Value> = mcp_tools().map(tool_definition).collect();
    let tools = tools_json(&server);
    assert_eq!(tools, expected);
    assert!(lint_tools(&tools).is_empty());
    let text = serde_json::to_string(&tools).unwrap();
    assert!(!text.contains("session_id"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_only_hides_tools_that_change_files() {
    let dir = scratch("readonly");
    let server = test_server(&dir.join("root"), true);
    let names: Vec<_> = server.commands.iter().map(|c| c.name).collect();
    assert_eq!(names, ["overview", "read", "eval"]);
    for t in tools_json(&server) {
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{}", t["name"]);
    }
    let r = server.call("edit", Some(Map::new()));
    assert_eq!(r.is_error, Some(true));
    let e = &r.structured_content.unwrap()["error"];
    assert!(e["what"].as_str().unwrap().contains("read-only"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn results_carry_the_same_json_as_text_and_structured() {
    let dir = scratch("results");
    let server = test_server(&dir.join("root"), false);
    std::fs::write(dir.join("root/a.csv"), b"Item,Price\nTea,3\n").unwrap();
    let mut args = Map::new();
    args.insert("path".into(), json!("a.csv"));
    let r = server.call("overview", Some(args));
    assert_eq!(r.is_error, Some(false));
    let structured = r.structured_content.clone().unwrap();
    assert_eq!(structured["ok"], true);
    assert!(structured["sheets"].is_array(), "{structured}");
    let text = &r.content[0].as_text().unwrap().text;
    assert_eq!(text, &structured.to_string());
    assert!(!text.contains('\n'));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failures_put_the_fix_first() {
    let dir = scratch("failures");
    let server = test_server(&dir.join("root"), false);
    for (name, args) in [
        ("overview", json!({ "path": "book.xlsx" })),
        ("overview", json!({ "path": "../outside.xlsx" })),
        ("overview", json!({ "path": "book.xlsx", "session_id": "x" })),
        ("overview", json!({})),
        ("overview", json!({ "path": format!("../{}", "x".repeat(5000)) })),
        ("overview", json!({ "path": "y".repeat(5000) })),
        ("nope", json!({})),
    ] {
        let r = server.call(name, args.as_object().cloned());
        assert_eq!(r.is_error, Some(true), "{name} {args}");
        let text = &r.content[0].as_text().unwrap().text;
        assert_eq!(text, &r.structured_content.unwrap().to_string());
        // The command line's key order, with the fix early.
        let code_at = text.find(r#""code":""#).unwrap();
        let what_at = text.find(r#""what":""#).unwrap();
        let fix_at = text.find(r#""fix":""#).unwrap();
        let why_at = text.find(r#""why":""#).unwrap();
        assert!(code_at < what_at && what_at < fix_at && fix_at < why_at, "{text}");
        assert!(fix_at < 200, "{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paths_are_confined_and_resolved() {
    let dir = scratch("confine");
    let server = test_server(&dir.join("root"), false);
    let r = server.call("overview", json!({ "path": "../outside.xlsx" }).as_object().cloned());
    assert_eq!(r.structured_content.unwrap()["error"]["code"], "outside_root");
    // Inside the root, the call reaches the kernel, which finds no workbook.
    let r = server.call("overview", json!({ "path": "book.xlsx" }).as_object().cloned());
    assert_eq!(r.structured_content.unwrap()["error"]["code"], "damaged_file");
    // A missing file inside the root is reported as missing, with ~.
    let r = server.call("read", json!({ "path": "gone.xlsx" }).as_object().cloned());
    assert_eq!(r.structured_content.unwrap()["error"]["code"], "file_not_found");
    // Outputs are confined too.
    let r = server.call(
        "convert",
        json!({ "path": "book.xlsx", "output": "../out.csv" }).as_object().cloned(),
    );
    assert_eq!(r.structured_content.unwrap()["error"]["code"], "outside_root");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn log_lines_escape_tag_characters_and_backslashes() {
    let line = log_line("x\u{E0041}\u{202E}\u{1b}[31m\\u{41}");
    assert_eq!(line, "carrysheet-cli: x\\u{e0041}\\u{202e}\\u{1b}[31m\\\\u{41}\n");
}

#[test]
fn log_lines_are_cleaned_like_file_text() {
    let line = log_line("forged\r\n2026-01-01 carrysheet-cli: \u{1b}[31mred\u{202e}");
    assert_eq!(line.matches('\n').count(), 1, "{line}");
    assert!(!line.contains('\r') && !line.contains('\u{1b}') && !line.contains('\u{202e}'));
    assert!(line.contains(r"forged\u{d}\u{a}2026"), "{line}");
    if let Some(home) = home_dir() {
        let line = log_line(&format!("opened {}", home.join("a.xlsx").display()));
        assert!(!line.contains(&home.display().to_string()), "{line}");
    }
    assert!(log_line(&"x".repeat(5_000)).len() < MAX_LOG_LINE + 60);
}
