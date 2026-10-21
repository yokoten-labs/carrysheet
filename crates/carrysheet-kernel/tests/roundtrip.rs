//! The core path end to end: open, read, edit, calculate, save, reopen.

use std::path::{Path, PathBuf};

use carrysheet_commands::engine::run;
use carrysheet_commands::{COMMANDS, CommandError, ErrorCode, validate_args};
use carrysheet_kernel::Kernel;
use serde_json::{Map, Value, json};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("carrysheet-kernel-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs a command the way the CLI and MCP do: validated arguments, then the
/// command layer on the kernel.
fn call(name: &str, args: Value) -> Result<Map<String, Value>, CommandError> {
    let command = COMMANDS.iter().find(|c| c.name == name).unwrap();
    let args = validate_args(command, &args)?;
    run::run(&Kernel::new(), command, &args)
}

fn p(path: &Path) -> String {
    path.display().to_string()
}

fn cells(page: &Map<String, Value>) -> Vec<Vec<Value>> {
    page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["cells"].as_array().unwrap().clone())
        .collect()
}

#[test]
fn open_read_edit_eval_save_reopen() {
    let dir = scratch("roundtrip");
    let csv = dir.join("prices.csv");
    std::fs::write(&csv, "Item,Price\nTea,3\nCake,4.5\nJam,2\n").unwrap();

    // csv -> xlsx.
    let book = dir.join("prices.xlsx");
    let done = call("convert", json!({ "path": p(&csv), "output": p(&book) })).unwrap();
    assert_eq!(done["written"], true, "{done:?}");
    assert_eq!(done["save"]["lost"], json!([]));

    let o = call("overview", json!({ "path": p(&book) })).unwrap();
    assert_eq!(o["sheets"][0]["used_range"], "A1:B4");
    assert_eq!(o["sheets"][0]["header"]["cells"], json!(["Item", "Price"]));

    // Values keep their types: "3" from csv is a number.
    let page = call("read", json!({ "path": p(&book), "range": "A1:B4" })).unwrap();
    assert_eq!(cells(&page)[1], vec![json!("Tea"), json!(3.0)]);

    // Edit: text stays text, numbers are numbers, a formula over a range.
    let edited = dir.join("prices-2.xlsx");
    let ops = json!([
        { "op": "set_value", "target": "B2", "value": 5 },
        { "op": "set_value", "target": "A5", "value": "=not a formula" },
        { "op": "set_value", "target": "B5", "value": "007" },
        { "op": "set_formula", "target": "C2:C4", "value": "=B2*2" },
        { "op": "set_formula", "target": "B6", "value": "SUM(B2:B4)" },
    ]);
    let dry = call(
        "edit",
        json!({ "path": p(&book), "ops": ops, "output": p(&edited), "dry_run": true }),
    )
    .unwrap();
    assert_eq!(dry["written"], false);
    assert!(!edited.exists(), "dry_run writes nothing");
    assert_eq!(dry["changes_total"], 7);
    assert_eq!(
        dry["changes"][0],
        json!({ "sheet": "prices", "cell": "B2", "before": 3.0, "after": 5.0 })
    );

    let done = call(
        "edit",
        json!({ "path": p(&book), "ops": ops, "output": p(&edited) }),
    )
    .unwrap();
    assert_eq!(done["written"], true, "{done:?}");
    assert_eq!(done["save"]["lost"], json!([]));

    // Reopen: the same values, formulas computed.
    let page = call(
        "read",
        json!({ "path": p(&edited), "range": "A1:C6", "mode": "both" }),
    )
    .unwrap();
    let rows = cells(&page);
    assert_eq!(rows[1][1]["value"], json!(5.0));
    assert_eq!(rows[1][2], json!({ "value": 10.0, "formula": "=B2*2" }));
    assert_eq!(rows[3][2]["formula"], "=B4*2");
    assert_eq!(rows[4][0]["value"], "=not a formula");
    assert_eq!(rows[4][1]["value"], "007");
    assert_eq!(
        rows[5][1],
        json!({ "value": 11.5, "formula": "=SUM(B2:B4)" })
    );

    let e = call(
        "eval",
        json!({ "path": p(&edited), "formula": "=SUM(C2:C4)+COUNTA(A:A)" }),
    )
    .unwrap();
    assert_eq!(e["value"], json!(28.0));
    assert_eq!(e["kind"], "number");
    let e = call(
        "eval",
        json!({ "path": p(&edited), "formula": "SEQUENCE(2,2)" }),
    )
    .unwrap();
    assert_eq!(e["value"], json!([[1.0, 2.0], [3.0, 4.0]]));

    // Detailed reads carry types and formats.
    let page = call(
        "read",
        json!({ "path": p(&edited), "range": "B5", "response_format": "detailed" }),
    )
    .unwrap();
    assert_eq!(cells(&page)[0][0]["type"], "text");

    // A file is never replaced without overwrite.
    let e = call(
        "edit",
        json!({ "path": p(&book), "ops": ops, "output": p(&edited) }),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::FileExists);

    // Without output, edit writes beside the original.
    let done = call(
        "edit",
        json!({ "path": p(&book), "ops": [{ "op": "clear", "target": "A:A" }] }),
    )
    .unwrap();
    assert!(
        done["output"]
            .as_str()
            .unwrap()
            .ends_with("prices-edited.xlsx")
    );
    assert_eq!(done["changes_total"], 4);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rows_columns_and_sheets() {
    let dir = scratch("structure");
    let csv = dir.join("data.csv");
    std::fs::write(&csv, "a,b\n1,2\n3,4\n").unwrap();
    let book = dir.join("data.xlsx");
    call("convert", json!({ "path": p(&csv), "output": p(&book) })).unwrap();
    std::fs::remove_file(&csv).unwrap();

    let out = dir.join("out.xlsx");
    let done = call(
        "edit",
        json!({
            "path": p(&book),
            "output": p(&out),
            "confirm": true,
            "ops": [
                { "op": "set_formula", "target": "C2", "value": "A2+B2" },
                { "op": "insert_rows", "target": "2:3" },
                { "op": "insert_columns", "target": "A:A", "count": 1 },
                { "op": "add_sheet", "target": "Notes" },
                { "op": "set_value", "target": "Notes!A1", "value": true },
                { "op": "rename_sheet", "target": "data", "value": "Data" },
                { "op": "delete_rows", "target": "6:6" },
            ],
        }),
    )
    .unwrap();
    assert_eq!(done["written"], true, "{done:?}");
    assert_eq!(done["ops"][1]["done"], "2 row(s) inserted before row 2");
    let o = call("overview", json!({ "path": p(&out) })).unwrap();
    let names: Vec<_> = o["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(names, [json!("Data"), json!("Notes")]);
    let page = call(
        "read",
        json!({ "path": p(&out), "range": "Data!A1:D5", "mode": "formulas" }),
    )
    .unwrap();
    let rows = cells(&page);
    assert_eq!(
        rows[0],
        vec![Value::Null, json!("a"), json!("b"), Value::Null]
    );
    assert_eq!(
        rows[3][3], "=B4+C4",
        "the formula moved with its row and column"
    );
    let page = call("read", json!({ "path": p(&out), "range": "Notes!A1" })).unwrap();
    assert_eq!(cells(&page)[0][0], json!(true));

    // A batch is all or nothing.
    let bad = dir.join("bad.xlsx");
    let e = call(
        "edit",
        json!({
            "path": p(&book),
            "output": p(&bad),
            "ops": [
                { "op": "set_value", "target": "A1", "value": 1 },
                { "op": "set_value", "target": "Nope!A1", "value": 1 },
            ],
        }),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::NoSuchSheet);
    assert!(e.what.contains("ops[1]"), "{}", e.what);
    assert!(!bad.exists());

    let e = call(
        "edit",
        json!({ "path": p(&book), "output": p(&bad), "ops": [{ "op": "set_formula", "target": "A1", "value": "WEBSERVICE(\"http://x\")" }] }),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    let e = call(
        "edit",
        json!({ "path": p(&book), "output": p(&bad), "ops": [{ "op": "set_formula", "target": "A1", "value": "SUM(1,2" }] }),
    );
    // Excel closes a missing parenthesis, and so does the kernel.
    assert!(e.is_ok(), "{e:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reading_pages_and_errors() {
    let dir = scratch("pages");
    let csv = dir.join("grid.csv");
    let mut text = String::new();
    for r in 0..250 {
        let row: Vec<String> = (0..60).map(|c| (r * 100 + c).to_string()).collect();
        text.push_str(&row.join(","));
        text.push('\n');
    }
    std::fs::write(&csv, text).unwrap();
    let page = call("read", json!({ "path": p(&csv) })).unwrap();
    assert_eq!(page["range"], "A1:AX200");
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    assert!(page["hint"].as_str().unwrap().contains("next_cursor"));
    let page = call("read", json!({ "path": p(&csv), "cursor": cursor })).unwrap();
    assert_eq!(page["range"], "AY1:BH200");

    let e = call("read", json!({ "path": p(&csv), "range": "Nope!A1" })).unwrap_err();
    assert_eq!(e.code, ErrorCode::NoSuchSheet);
    let e = call("eval", json!({ "path": p(&csv), "formula": "1/0" })).unwrap();
    assert_eq!(
        (e["kind"].as_str(), e["value"].as_str()),
        (Some("error"), Some("#DIV/0!"))
    );
    let e = call("eval", json!({ "path": p(&csv), "formula": "1+*2" })).unwrap_err();
    assert_eq!(e.code, ErrorCode::Formula);
    let e = call("overview", json!({ "path": p(&dir.join("x.ods")) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedFormat);
    let e = call("overview", json!({ "path": p(&dir.join("gone.xlsx")) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::FileNotFound);
    let e = call(
        "convert",
        json!({ "path": p(&csv), "output": p(&dir.join("x.xls")) }),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotImplemented);

    let _ = std::fs::remove_dir_all(&dir);
}
