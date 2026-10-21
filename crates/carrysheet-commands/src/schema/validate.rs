//! Checks arguments against the command table on our side. Clients may skip
//! schema validation, so every surface (CLI, MCP) calls this before running
//! a command.

use carrysheet_model::SheetRange;
use serde_json::{Map, Value};

use crate::error::{CommandError, ErrorCode};
use crate::table::{Arg, ArgType, Command, DESTRUCTIVE_EDIT_OPS};

/// The most items a list argument may hold.
pub const MAX_LIST_ITEMS: usize = 10_000;

/// The most characters a text argument may hold (formulas are limited to
/// 8,192 in Excel; this leaves room for long cell text).
pub const MAX_TEXT_CHARS: usize = 32_767;

/// Checks `args` (a JSON object) against a command's arguments. Missing and
/// `null` mean "not given". Returns the arguments with every known name
/// present (absent ones as `null`), so handlers can index them directly.
pub fn validate_args(command: &Command, args: &Value) -> Result<Map<String, Value>, CommandError> {
    let Some(map) = args.as_object() else {
        return Err(CommandError::invalid_argument(
            "arguments",
            "the arguments must be a JSON object",
            format!(
                "Send an object such as {{\"path\": \"book.xlsx\"}}. Run `carrysheet-cli {} --help` for the arguments.",
                command.cli_name()
            ),
        ));
    };
    check_object(&command.all_args(), map, "", &command.cli_name())
}

fn check_object(
    fields: &[&Arg],
    map: &Map<String, Value>,
    prefix: &str,
    cli_name: &str,
) -> Result<Map<String, Value>, CommandError> {
    if let Some(unknown) = map
        .keys()
        .find(|k| !fields.iter().any(|f| f.name == k.as_str()))
    {
        let known: Vec<_> = fields.iter().map(|f| f.name).collect();
        return Err(CommandError::invalid_argument(
            &format!("{prefix}{unknown}"),
            format!("`{cli_name}` has no argument named `{unknown}`"),
            format!("Use one of: {}.", known.join(", ")),
        ));
    }
    let mut out = Map::new();
    for field in fields {
        let name = format!("{prefix}{}", field.name);
        let value = map.get(field.name).unwrap_or(&Value::Null);
        if value.is_null() {
            if field.required {
                return Err(CommandError::invalid_argument(
                    &name,
                    format!("`{name}` is required: {}", field.description),
                    format!("Give a value for `{name}`."),
                ));
            }
            out.insert(field.name.into(), Value::Null);
            continue;
        }
        out.insert(
            field.name.into(),
            check_value(field, value, &name, cli_name)?,
        );
    }
    Ok(out)
}

fn wrong(name: &str, expected: &str, got: &Value) -> CommandError {
    let got = match got {
        Value::Null => "nothing".to_string(),
        Value::Bool(_) => "true or false".to_string(),
        Value::Number(n) => format!("the number {n}"),
        Value::String(s) if s.chars().count() > 40 => "a long text".to_string(),
        Value::String(s) => format!("the text \"{s}\""),
        Value::Array(_) => "a list".to_string(),
        Value::Object(_) => "an object".to_string(),
    };
    CommandError::invalid_argument(
        name,
        format!("`{name}` must be {expected}, but it is {got}"),
        format!("Give `{name}` as {expected}."),
    )
}

fn check_value(
    arg: &Arg,
    value: &Value,
    name: &str,
    cli_name: &str,
) -> Result<Value, CommandError> {
    match arg.ty {
        ArgType::Text | ArgType::Path | ArgType::Range => {
            let Some(s) = value.as_str() else {
                return Err(wrong(name, "text", value));
            };
            if s.chars().count() > MAX_TEXT_CHARS {
                return Err(CommandError::invalid_argument(
                    name,
                    format!("`{name}` is longer than {MAX_TEXT_CHARS} characters"),
                    "Shorten it.",
                ));
            }
            if arg.ty == ArgType::Path && (s.is_empty() || s.contains('\0')) {
                return Err(CommandError::invalid_argument(
                    name,
                    format!("`{name}` is not a usable path"),
                    "Give a file path like book.xlsx or reports/q1.xlsx.",
                ));
            }
            if arg.ty == ArgType::Range {
                SheetRange::parse(s)?;
            }
            Ok(value.clone())
        }
        ArgType::Integer { min, max } => {
            let expected = format!("a whole number from {min} to {max}");
            match value.as_i64() {
                Some(n) if (min..=max).contains(&n) => Ok(value.clone()),
                _ => Err(wrong(name, &expected, value)),
            }
        }
        ArgType::Boolean => match value {
            Value::Bool(_) => Ok(value.clone()),
            _ => Err(wrong(name, "true or false", value)),
        },
        // Kept exactly as sent: the JSON type says whether "3" is text or a number.
        ArgType::Scalar => match value {
            Value::String(s) if s.chars().count() > MAX_TEXT_CHARS => {
                Err(CommandError::invalid_argument(
                    name,
                    format!("`{name}` is longer than {MAX_TEXT_CHARS} characters"),
                    "Shorten it.",
                ))
            }
            Value::String(_) | Value::Number(_) | Value::Bool(_) => Ok(value.clone()),
            _ => Err(wrong(name, "text, a number, or true or false", value)),
        },
        ArgType::Enum(values) => {
            let expected = format!("one of {}", values.join(", "));
            match value.as_str() {
                Some(s) if values.contains(&s) => Ok(value.clone()),
                _ => Err(wrong(name, &expected, value)),
            }
        }
        ArgType::TextList => {
            let list = check_list(value, name, "a list of text")?;
            for (i, item) in list.iter().enumerate() {
                if !item.is_string() {
                    return Err(wrong(&format!("{name}[{i}]"), "text", item));
                }
            }
            Ok(value.clone())
        }
        ArgType::ObjectList(fields) => {
            let list = check_list(value, name, "a list of objects")?;
            let fields: Vec<&Arg> = fields.iter().collect();
            let mut out = Vec::with_capacity(list.len());
            for (i, item) in list.iter().enumerate() {
                let item_name = format!("{name}[{i}]");
                let Some(obj) = item.as_object() else {
                    return Err(wrong(&item_name, "an object", item));
                };
                out.push(Value::Object(check_object(
                    &fields,
                    obj,
                    &format!("{item_name}."),
                    cli_name,
                )?));
            }
            Ok(Value::Array(out))
        }
    }
}

/// Checks that a call which deletes data or overwrites a file says
/// `confirm: true`. `args` are validated arguments ([`validate_args`]).
/// `dry_run: true` never needs it, because nothing is written.
pub fn check_confirmation(
    command: &Command,
    args: &Map<String, Value>,
) -> Result<(), CommandError> {
    let is_true = |name: &str| args.get(name) == Some(&Value::Bool(true));
    if command.arg("confirm").is_none() || is_true("confirm") || is_true("dry_run") {
        return Ok(());
    }
    let mut actions: Vec<String> = Vec::new();
    if let Some(ops) = args.get("ops").and_then(Value::as_array) {
        for (i, op) in ops.iter().enumerate() {
            if let Some(name) = op.get("op").and_then(Value::as_str)
                && DESTRUCTIVE_EDIT_OPS.contains(&name)
            {
                actions.push(format!("{} (ops[{i}])", name.replace('_', " ")));
            }
        }
    }
    if is_true("overwrite") {
        actions.push("overwrite a file".to_owned());
    }
    let Some(first) = actions.first() else {
        return Ok(());
    };
    let more = match actions.len() {
        1 => String::new(),
        n => format!(" and {} more", n - 1),
    };
    Err(CommandError::new(
        ErrorCode::ConfirmationRequired,
        format!(
            "`{}` would {first}{more}; confirm is not true",
            command.cli_name()
        ),
        "deleting sheets, rows or columns and overwriting files cannot be undone, so they need confirm: true. dry_run lists the changes without it.",
        format!(
            "Call again with confirm set to true: {}. On the command line, add --confirm.",
            call_with_confirm(args)
        ),
    ))
}

/// The same call with `confirm: true`, as compact JSON. Long lists are
/// summed up, so the example stays short enough to read.
fn call_with_confirm(args: &Map<String, Value>) -> String {
    // Validation fills in absent fields as null; leave them out again.
    fn without_nulls(value: &Value) -> Value {
        match value {
            Value::Array(items) => Value::Array(items.iter().map(without_nulls).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| (k.clone(), without_nulls(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    let mut parts = Vec::new();
    for (name, value) in args {
        if value.is_null() || name == "confirm" {
            continue;
        }
        let value = &without_nulls(value);
        let shown = match value {
            Value::Array(items) if value.to_string().chars().count() > 160 => {
                format!("[...the same {} items...]", items.len())
            }
            Value::String(s) if s.chars().count() > 80 => "\"...\"".to_owned(),
            _ => value.to_string(),
        };
        parts.push(format!("\"{name}\":{shown}"));
    }
    parts.push("\"confirm\":true".to_owned());
    format!("{{{}}}", parts.join(","))
}

fn check_list<'a>(
    value: &'a Value,
    name: &str,
    expected: &str,
) -> Result<&'a Vec<Value>, CommandError> {
    let Some(list) = value.as_array() else {
        return Err(wrong(name, expected, value));
    };
    if list.len() > MAX_LIST_ITEMS {
        return Err(CommandError::invalid_argument(
            name,
            format!(
                "`{name}` has {} items; the limit is {MAX_LIST_ITEMS}",
                list.len()
            ),
            "Split the work into several calls.",
        ));
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorCode, ExitCode};
    use crate::table::command;
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

        let e =
            validate_args(edit, &json!({ "path": "a", "ops": [{ "op": "clear" }] })).unwrap_err();
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
            e.fix.starts_with(
                "Call again with confirm set to true: {\"path\":\"book.xlsx\",\"ops\":[{"
            ),
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
}
