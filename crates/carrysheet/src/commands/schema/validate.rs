//! Checks arguments against the command table on our side. Clients may skip
//! schema validation, so every surface (CLI, MCP) calls this before running
//! a command.

use crate::range::SheetRange;
use serde_json::{Map, Value};

use crate::commands::error::{CommandError, ErrorCode};
use crate::commands::table::{Arg, ArgType, Command, DESTRUCTIVE_EDIT_OPS};

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
