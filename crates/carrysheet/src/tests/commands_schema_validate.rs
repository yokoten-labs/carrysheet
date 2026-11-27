use crate::commands::error::{ErrorCode, ExitCode};
use crate::commands::schema::validate::*;
use crate::commands::table::Command;
use crate::commands::table::command;
use serde_json::Value;
use serde_json::json;

fn read() -> &'static Command {
    command("read").unwrap()
}

#[test]
fn fills_missing_optional_args_with_null() {
    let out = validate_args(read(), &json!({ "path": "a.xlsx" })).unwrap();
    assert_eq!(out["path"], "a.xlsx");
    assert_eq!(out["range"], Value::Null);
    assert_eq!(out["max_rows"], Value::Null);
    assert_eq!(out.len(), read().all_args().len());
}

#[test]
fn accepts_good_values() {
    let out = validate_args(
        read(),
        &json!({ "path": "a.xlsx", "range": "'Q1 売上'!A1:C3", "mode": "both", "max_rows": 50, "cursor": null }),
    )
    .unwrap();
    assert_eq!(out["mode"], "both");
}

#[test]
fn refuses_missing_required() {
    let e = validate_args(read(), &json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(e.exit_code(), ExitCode::Usage);
    assert!(e.why.contains("`path` is required"));
    let e = validate_args(read(), &json!({ "path": null })).unwrap_err();
    assert!(e.why.contains("required"));
}

#[test]
fn refuses_unknown_args_and_lists_known_ones() {
    let e = validate_args(read(), &json!({ "path": "a", "sheet_id": 1 })).unwrap_err();
    assert!(e.why.contains("sheet_id"));
    assert!(e.fix.contains("max_rows"));
}

#[test]
fn refuses_wrong_types() {
    for bad in [
        json!({ "path": 1 }),
        json!({ "path": "" }),
        json!({ "path": "a\u{0}b" }),
        json!({ "path": "a", "max_rows": 0 }),
        json!({ "path": "a", "max_rows": 10_001 }),
        json!({ "path": "a", "max_rows": 1.5 }),
        json!({ "path": "a", "max_rows": "5" }),
        json!({ "path": "a", "mode": "everything" }),
        json!({ "path": "a", "mode": 1 }),
        json!(["a"]),
    ] {
        let e = validate_args(read(), &bad).unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::Usage, "{bad}");
    }
    let e = validate_args(read(), &json!({ "path": "a", "max_rows": 0 })).unwrap_err();
    assert!(e.why.contains("from 1 to 10000"), "{}", e.why);
}

#[test]
fn checks_ranges() {
    let e = validate_args(read(), &json!({ "path": "a", "range": "A1:XFE1" })).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidReference);
    assert_eq!(e.exit_code(), ExitCode::Usage);
}

#[test]
fn checks_booleans() {
    let convert = command("convert").unwrap();
    assert!(
        validate_args(
            convert,
            &json!({ "path": "a", "output": "b.csv", "dry_run": true })
        )
        .is_ok()
    );
    assert!(
        validate_args(
            convert,
            &json!({ "path": "a", "output": "b", "dry_run": "yes" })
        )
        .is_err()
    );
}

#[test]
fn checks_object_lists() {
    let edit = command("edit").unwrap();
    let ok = validate_args(
        edit,
        &json!({ "path": "a", "ops": [{ "op": "set_value", "target": "A1", "value": "3" }] }),
    )
    .unwrap();
    assert_eq!(ok["ops"][0]["count"], Value::Null);

    let e = validate_args(
        edit,
        &json!({ "path": "a", "ops": [{ "op": "explode", "target": "A1" }] }),
    )
    .unwrap_err();
    assert!(e.what.contains("ops[0].op"), "{}", e.what);

    let e = validate_args(edit, &json!({ "path": "a", "ops": [{ "op": "clear" }] })).unwrap_err();
    assert!(e.why.contains("ops[0].target"), "{}", e.why);

    let e = validate_args(
        edit,
        &json!({ "path": "a", "ops": [{ "op": "clear", "target": "A1", "x": 1 }] }),
    )
    .unwrap_err();
    assert!(e.why.contains("`x`"), "{}", e.why);

    assert!(validate_args(edit, &json!({ "path": "a", "ops": ["clear"] })).is_err());

    // A value keeps the type it was sent with; nothing is guessed.
    let typed = validate_args(
        edit,
        &json!({ "path": "a", "ops": [
            { "op": "set_value", "target": "A1", "value": "3" },
            { "op": "set_value", "target": "A2", "value": 3 },
            { "op": "set_value", "target": "A3", "value": 2.5 },
            { "op": "set_value", "target": "A4", "value": true },
            { "op": "clear", "target": "A5", "value": null },
        ] }),
    )
    .unwrap();
    assert_eq!(typed["ops"][0]["value"], json!("3"));
    assert_eq!(typed["ops"][1]["value"], json!(3));
    assert_eq!(typed["ops"][2]["value"], json!(2.5));
    assert_eq!(typed["ops"][3]["value"], json!(true));
    assert_eq!(typed["ops"][4]["value"], Value::Null);
    for bad in [json!([3]), json!({ "n": 3 })] {
        let e = validate_args(
            edit,
            &json!({ "path": "a", "ops": [{ "op": "set_value", "target": "A1", "value": bad }] }),
        )
        .unwrap_err();
        assert!(e.why.contains("ops[0].value"), "{}", e.why);
        assert!(
            e.why.contains("text, a number, or true or false"),
            "{}",
            e.why
        );
    }
    let long = "z".repeat(MAX_TEXT_CHARS + 1);
    assert!(
        validate_args(
            edit,
            &json!({ "path": "a", "ops": [{ "op": "set_value", "target": "A1", "value": long }] }),
        )
        .is_err()
    );
    assert!(validate_args(edit, &json!({ "path": "a", "ops": {} })).is_err());
}

#[test]
fn limits_list_sizes_and_text_length() {
    let edit = command("edit").unwrap();
    let ops: Vec<Value> = (0..=MAX_LIST_ITEMS)
        .map(|_| json!({ "op": "clear", "target": "A1" }))
        .collect();
    let e = validate_args(edit, &json!({ "path": "a", "ops": ops })).unwrap_err();
    assert!(e.why.contains("limit"));
    let long = "x".repeat(MAX_TEXT_CHARS + 1);
    assert!(validate_args(read(), &json!({ "path": long })).is_err());
}

#[test]
fn deleting_and_overwriting_need_confirm() {
    let edit = command("edit").unwrap();
    let call = |extra: Value| {
        let mut args = json!({
            "path": "book.xlsx",
            "ops": [
                { "op": "set_value", "target": "A1", "value": 1 },
                { "op": "delete_rows", "target": "Sheet1!3:4" }
            ]
        });
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        let args = validate_args(edit, &args).unwrap();
        check_confirmation(edit, &args)
    };
    let e = call(json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfirmationRequired);
    assert_eq!(e.exit_code(), ExitCode::Failed);
    assert!(e.what.contains("delete rows (ops[1])"), "{}", e.what);
    assert!(
        e.fix
            .starts_with("Call again with confirm set to true: {\"path\":\"book.xlsx\",\"ops\":[{"),
        "{}",
        e.fix
    );
    assert!(e.fix.contains(r#""confirm":true}"#), "{}", e.fix);
    assert!(!e.fix.contains("null"), "{}", e.fix);
    // The fix comes within the first ~200 characters agents keep.
    let shown = e.to_json().to_string();
    assert!(shown.find("Call again").unwrap() < 200, "{shown}");

    let e = call(json!({ "overwrite": true })).unwrap_err();
    assert!(e.what.contains("and 1 more"), "{}", e.what);
    assert!(
        e.fix.contains(r#""overwrite":true,"confirm":true"#),
        "{}",
        e.fix
    );

    assert!(call(json!({ "confirm": true })).is_ok());
    assert!(call(json!({ "dry_run": true })).is_ok());
    assert!(call(json!({ "confirm": false })).is_err());

    // Nothing destructive: no confirmation.
    let args = validate_args(
        edit,
        &json!({ "path": "a", "ops": [{ "op": "clear", "target": "A1" }] }),
    )
    .unwrap();
    assert!(check_confirmation(edit, &args).is_ok());

    // Overwriting with convert.
    let convert = command("convert").unwrap();
    let args = validate_args(
        convert,
        &json!({ "path": "a.xlsx", "output": "b.csv", "overwrite": true }),
    )
    .unwrap();
    let e = check_confirmation(convert, &args).unwrap_err();
    assert!(
        e.what.starts_with("`convert` would overwrite a file"),
        "{}",
        e.what
    );

    // Long lists are summed up in the example.
    let ops: Vec<Value> = (0..50)
        .map(|i| json!({ "op": "delete_sheet", "target": format!("S{i}") }))
        .collect();
    let args = validate_args(edit, &json!({ "path": "a", "ops": ops })).unwrap();
    let e = check_confirmation(edit, &args).unwrap_err();
    assert!(e.fix.contains("[...the same 50 items...]"), "{}", e.fix);
    assert!(e.what.contains("and 49 more"), "{}", e.what);

    // Commands without confirm never need it.
    let read_args = validate_args(read(), &json!({ "path": "a" })).unwrap();
    assert!(check_confirmation(read(), &read_args).is_ok());
}

#[test]
fn long_text_is_not_echoed_back() {
    let long = "y".repeat(500);
    let e = validate_args(read(), &json!({ "path": "a", "mode": long })).unwrap_err();
    assert!(e.why.len() < 200, "{}", e.why);
}
