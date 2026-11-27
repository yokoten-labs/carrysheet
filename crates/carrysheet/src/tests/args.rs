use crate::args::*;
use crate::error::{ErrorCode, ExitCode};
use serde_json::{Value, json};
use std::collections::HashSet;

fn read() -> &'static Command {
    command("read").unwrap()
}

fn is_snake(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The argument names of a command, in order.
fn names(c: &Command) -> Vec<String> {
    let schema = input_schema(c);
    let names = schema["required"].as_array().unwrap().iter();
    names.map(|n| n.as_str().unwrap().to_owned()).collect()
}

// The commands.

#[test]
fn names_are_unique_snake_case() {
    let mut seen = HashSet::new();
    for c in COMMANDS {
        assert!(is_snake(c.name), "{}", c.name);
        assert!(seen.insert(c.name), "duplicate {}", c.name);
        assert!(std::ptr::eq(command(c.name).unwrap(), c));
        assert_eq!(c.cli_name().replace(' ', "_"), c.name);
    }
    assert_eq!(command("mcp_serve").unwrap().cli_name(), "mcp serve");
}

#[test]
fn mvp_commands_are_all_there() {
    for name in [
        "overview",
        "read",
        "eval",
        "convert",
        "edit",
        "mcp_serve",
        "commands",
    ] {
        assert!(command(name).is_some(), "{name}");
    }
}

#[test]
fn text_fits_help_and_tool_lists() {
    for c in COMMANDS {
        let (summary, description) = c.doc();
        assert!(!summary.is_empty() && summary.len() <= 72, "{}", c.name);
        assert!(!summary.ends_with('.') && !summary.contains('\n'), "{}", c.name);
        assert_ne!(summary, description, "{}: one doc paragraph", c.name);
        assert!(
            !description.is_empty() && description.len() <= 400,
            "{}",
            c.name
        );
        assert!(!description.contains('\n'), "{}", c.name);
        let schema = input_schema(c);
        for name in names(c) {
            assert!(is_snake(&name), "{}.{name}", c.name);
            let text = schema["properties"][&name]["description"].as_str();
            let text = text.unwrap_or_else(|| panic!("{}.{name} has no description", c.name));
            assert!(!text.is_empty() && text.len() <= 200, "{}.{name}", c.name);
        }
    }
}

#[test]
fn dry_run_is_last_on_commands_that_change_files() {
    for c in COMMANDS {
        assert_eq!(c.has_arg("dry_run"), c.changes_files(), "{}", c.name);
        if c.changes_files() {
            assert!(!c.is_read_only(), "{}", c.name);
            assert_eq!(names(c).last().unwrap(), "dry_run");
        }
    }
    assert!(command("convert").unwrap().changes_files());
    assert!(command("edit").unwrap().changes_files());
    assert!(!read().changes_files());
}

#[test]
fn deleting_and_overwriting_can_be_confirmed() {
    for c in COMMANDS {
        if c.has_arg("overwrite") {
            assert!(c.has_arg("confirm"), "{}", c.name);
            assert_eq!(c.annotations()["destructiveHint"], true, "{}", c.name);
        }
    }
    assert!(command("edit").unwrap().has_arg("confirm"));
}

#[test]
fn read_only_commands_are_not_destructive() {
    for c in COMMANDS {
        if c.is_read_only() {
            assert_eq!(c.annotations()["readOnlyHint"], true, "{}", c.name);
            assert_eq!(c.annotations()["destructiveHint"], false, "{}", c.name);
            assert!(!c.changes_files(), "{}", c.name);
        }
    }
}

#[test]
fn mcp_tools_in_fixed_order_and_few() {
    let tools: Vec<_> = mcp_tools().map(|c| c.name).collect();
    assert_eq!(tools, ["overview", "read", "eval", "convert", "edit"]);
}

#[test]
fn path_arguments_are_text() {
    let mut found = HashSet::new();
    for c in COMMANDS {
        let schema = input_schema(c);
        for name in PATHS.iter().filter(|name| c.has_arg(name)) {
            let property = &schema["properties"][*name];
            let kind = property.pointer("/anyOf/0").unwrap_or(property);
            assert_eq!(kind["type"], "string", "{}.{name}", c.name);
            found.insert(*name);
        }
    }
    assert_eq!(found.len(), PATHS.len(), "a name in PATHS is not an argument");
}

// The schemas.

#[test]
fn overview_schema_shape() {
    let s = input_schema(command("overview").unwrap());
    assert_eq!(s["type"], "object");
    assert_eq!(s["additionalProperties"], false);
    assert_eq!(s["required"], json!(["path", "response_format"]));
    assert_eq!(s["properties"]["path"]["type"], "string");
    let rf = &s["properties"]["response_format"];
    assert_eq!(rf["anyOf"][0]["enum"], json!(["concise", "detailed"]));
    assert_eq!(rf["anyOf"][1], json!({ "type": "null" }));
    assert!(rf["description"].is_string());
    assert!(rf.get("type").is_none());
    // The struct's doc comment is the tool description, not the schema's.
    assert!(s.get("description").is_none());
    let keys: Vec<_> = s.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["type", "properties", "required", "additionalProperties"]);
}

#[test]
fn schemas_keep_only_what_every_client_accepts() {
    let s = input_schema(read());
    assert_eq!(names(read()), ["path", "range", "mode", "max_rows", "max_columns", "cursor", "response_format"]);
    // No `format`, `minimum` or `default` from schemars, and no type lists.
    assert_eq!(
        s["properties"]["max_rows"],
        json!({
            "anyOf": [{ "type": "integer" }, { "type": "null" }],
            "description": "Rows per page, 1 to 10000. Default: 200."
        })
    );
    assert_eq!(
        s["properties"]["mode"]["anyOf"][0],
        json!({ "type": "string", "enum": ["values", "formulas", "both"] })
    );
    let s = input_schema(command("convert").unwrap());
    assert_eq!(
        s["properties"]["dry_run"]["anyOf"],
        json!([{ "type": "boolean" }, { "type": "null" }])
    );
    assert_eq!(
        s["properties"]["format"]["anyOf"][0]["enum"],
        json!(["xlsx", "xls", "csv"])
    );
    assert_eq!(
        input_schema(command("commands").unwrap()),
        json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false })
    );
}

#[test]
fn edit_ops_are_strict_objects() {
    let s = input_schema(command("edit").unwrap());
    let ops = &s["properties"]["ops"];
    let keys: Vec<_> = ops.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["type", "items", "description"]);
    let items = &ops["items"];
    assert_eq!(ops["type"], "array");
    assert_eq!(items["type"], "object");
    assert_eq!(items["additionalProperties"], false);
    assert!(items.get("description").is_none());
    assert_eq!(items["required"], json!(["op", "target", "value", "count"]));
    assert_eq!(
        items["properties"]["op"]["enum"],
        json!([
            "set_value",
            "set_formula",
            "clear",
            "insert_rows",
            "delete_rows",
            "insert_columns",
            "delete_columns",
            "add_sheet",
            "rename_sheet",
            "delete_sheet"
        ])
    );
    assert_eq!(
        items["properties"]["value"]["anyOf"],
        json!([
            { "type": "string" },
            { "type": "number" },
            { "type": "boolean" },
            { "type": "null" }
        ])
    );
}

#[test]
fn tool_definitions_have_all_annotations() {
    for c in COMMANDS {
        let t = tool_definition(c);
        assert_eq!(t["name"], c.name);
        assert_eq!(t["description"], c.doc().1);
        let keys: Vec<_> = t["annotations"].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "readOnlyHint",
                "destructiveHint",
                "idempotentHint",
                "openWorldHint"
            ]
        );
        assert!(t["annotations"].as_object().unwrap().values().all(Value::is_boolean));
    }
}

// Checking what a client sent.

#[test]
fn fills_missing_optional_args_with_null() {
    let out = validate_args(read(), &json!({ "path": "a.xlsx" })).unwrap();
    assert_eq!(out["path"], "a.xlsx");
    assert_eq!(out["range"], Value::Null);
    assert_eq!(out["max_rows"], Value::Null);
    let keys: Vec<_> = out.keys().cloned().collect();
    assert_eq!(keys, names(read()));
}

#[test]
fn accepts_good_values() {
    let out = validate_args(
        read(),
        &json!({ "path": "a.xlsx", "range": "'Q1 売上'!A1:C3", "mode": "both", "max_rows": 50, "cursor": null }),
    )
    .unwrap();
    assert_eq!(out["mode"], "both");
    assert_eq!(out["max_rows"], 50);
    assert_eq!(out["cursor"], Value::Null);
}

#[test]
fn refuses_missing_required() {
    let e = validate_args(read(), &json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(e.exit_code(), ExitCode::Usage);
    assert!(e.what.contains("`path`"), "{}", e.what);
    assert!(e.why.contains("missing field `path`"), "{}", e.why);
    assert!(e.fix.contains("The workbook"), "{}", e.fix);
    let e = validate_args(read(), &json!({ "path": null })).unwrap_err();
    assert!(e.why.contains("missing field `path`"));
    let eval = command("eval").unwrap();
    let e = validate_args(eval, &json!({ "path": "a" })).unwrap_err();
    assert!(e.what.contains("`formula`"), "{}", e.what);
}

#[test]
fn refuses_unknown_args_and_lists_known_ones() {
    let e = validate_args(read(), &json!({ "path": "a", "sheet_id": 1 })).unwrap_err();
    assert!(e.what.contains("sheet_id"));
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
        json!({ "path": "a", "max_rows": -1 }),
        json!({ "path": "a", "max_rows": "5" }),
        json!({ "path": "a", "mode": "everything" }),
        json!({ "path": "a", "mode": 1 }),
        json!(["a"]),
        json!(null),
    ] {
        let e = validate_args(read(), &bad).unwrap_err();
        assert_eq!(e.exit_code(), ExitCode::Usage, "{bad}");
    }
    let e = validate_args(read(), &json!({ "path": "a", "max_rows": 0 })).unwrap_err();
    assert!(e.why.contains("from 1 to 10000"), "{}", e.why);
}

#[test]
fn a_wrong_value_names_its_argument() {
    for (bad, name) in [
        (json!({ "path": 1 }), "`path`"),
        (json!({ "max_rows": "5", "path": "a" }), "`max_rows`"),
        (json!({ "path": "a", "cursor": "c", "mode": 1 }), "`mode`"),
        (json!({ "path": "a", "max_columns": 1.5, "mode": "all" }), "`max_columns`"),
        (json!({ "path": "a", "max_columns": 0 }), "`max_columns`"),
    ] {
        let e = validate_args(read(), &bad).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument, "{bad}");
        assert!(e.what.contains(name), "{bad}: {}", e.what);
    }
    // The fix is the argument's own description.
    let e = validate_args(read(), &json!({ "path": "a", "max_rows": "5" })).unwrap_err();
    assert!(e.fix.contains("Rows per page"), "{}", e.fix);
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
    let ok = validate_args(
        convert,
        &json!({ "path": "a", "output": "b.csv", "dry_run": true, "confirm": false }),
    )
    .unwrap();
    assert_eq!(ok["dry_run"], true);
    assert_eq!(ok["confirm"], false);
    assert_eq!(ok["overwrite"], Value::Null);
    let e = validate_args(
        convert,
        &json!({ "path": "a", "output": "b", "dry_run": "yes" }),
    )
    .unwrap_err();
    assert!(e.what.contains("`dry_run`"), "{}", e.what);
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

    let good = json!({ "op": "clear", "target": "A1" });
    let e = validate_args(
        edit,
        &json!({ "path": "a", "ops": [good, { "op": "explode", "target": "A1" }] }),
    )
    .unwrap_err();
    assert!(e.what.contains("`ops[1]`"), "{}", e.what);
    assert!(e.why.contains("explode"), "{}", e.why);

    let e = validate_args(edit, &json!({ "path": "a", "ops": [{ "op": "clear" }] })).unwrap_err();
    assert!(e.what.contains("`ops[0]`"), "{}", e.what);
    assert!(e.why.contains("`target`"), "{}", e.why);
    assert!(e.fix.contains("The operations"), "{}", e.fix);

    let e = validate_args(
        edit,
        &json!({ "path": "a", "ops": [{ "op": "clear", "target": "A1", "x": 1 }] }),
    )
    .unwrap_err();
    assert!(e.why.contains("`x`"), "{}", e.why);

    let e = validate_args(
        edit,
        &json!({ "path": "a", "ops": [{ "op": "insert_rows", "target": "2:2", "count": 0 }] }),
    )
    .unwrap_err();
    assert!(e.what.contains("`ops[0].count`"), "{}", e.what);
    assert!(e.why.contains("from 1 to 1048576"), "{}", e.why);

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
    assert!(typed["ops"][1]["value"].is_u64());
    assert_eq!(typed["ops"][2]["value"], json!(2.5));
    assert_eq!(typed["ops"][3]["value"], json!(true));
    assert_eq!(typed["ops"][4]["value"], Value::Null);
    for bad in [json!([3]), json!({ "n": 3 })] {
        let e = validate_args(
            edit,
            &json!({ "path": "a", "ops": [{ "op": "set_value", "target": "A1", "value": bad }] }),
        )
        .unwrap_err();
        assert!(e.what.contains("`ops[0]`"), "{}", e.what);
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
    assert!(validate_args(edit, &json!({ "path": "a" })).is_err());
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
                { "op": "delete_rows", "target": "Sheet1!3:4", "count": null }
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

    // Each deleting operation is named.
    for (op, shown) in [
        ("delete_rows", "delete rows"),
        ("delete_columns", "delete columns"),
        ("delete_sheet", "delete sheet"),
    ] {
        let ops = json!([{ "op": op, "target": "S" }]);
        let args = validate_args(edit, &json!({ "path": "a", "ops": ops })).unwrap();
        let e = check_confirmation(edit, &args).unwrap_err();
        assert!(e.what.contains(&format!("{shown} (ops[0])")), "{}", e.what);
    }

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
