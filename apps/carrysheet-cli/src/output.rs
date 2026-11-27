//! Printing results and errors. With `--json`, stdout holds one JSON
//! document and nothing else; messages for people go to stderr.

use std::io::Write;

use carrysheet::commands::{CommandError, ErrorCode};
use carrysheet::text::escape;
use clap::error::ErrorKind;
use serde_json::{Map, Value, json};

use crate::cli::BIN;
use crate::files::{redact_error, redact_home};
use crate::run::Done;

/// The only writer to stdout (clippy's `disallowed-methods` keeps it so); ignores a closed pipe.
#[expect(clippy::disallowed_methods, reason = "the one writer to stdout")]
fn stdout_line(text: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

/// The success envelope: `{"ok": true, ...data}`.
pub fn ok_envelope(data: &Map<String, Value>) -> Value {
    let mut map = Map::from_iter([("ok".to_owned(), Value::Bool(true))]);
    map.extend(data.clone());
    Value::Object(map)
}

/// Prints a result: the JSON envelope, or the one text form for people.
pub fn print_done(done: &Done, json: bool) {
    if json {
        stdout_line(&ok_envelope(&done.data).to_string());
    } else {
        stdout_line(render(done.plain.as_ref().unwrap_or(&done.data)).trim_end());
    }
}

/// A value on one line. Text is cleaned and shows `~` for the home folder.
fn scalar(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => escape(&redact_home(s), 2000),
        Value::Number(n) if !n.is_f64() => n.to_string(),
        Value::Number(n) => n.as_f64().unwrap_or_default().to_string(),
        other => escape(&redact_home(&other.to_string()), 2000),
    }
}

/// One table field; a list of plain values spreads over several fields.
fn cell(value: &Value) -> String {
    match value.as_array() {
        Some(list) if list.iter().all(|v| !v.is_object() && !v.is_array()) => {
            list.iter().map(scalar).collect::<Vec<_>>().join("\t")
        }
        _ => scalar(value),
    }
}

fn lines(key: &str, value: &Value, pad: &str, out: &mut String) {
    if value.is_null() {
        return;
    }
    let rows = value.as_array().filter(|l| !l.is_empty() && l.iter().all(Value::is_object));
    if let Some(rows) = rows {
        let mut keys: Vec<&str> = Vec::new();
        for k in rows.iter().flat_map(|r| r.as_object().unwrap().keys()) {
            if !keys.contains(&k.as_str()) {
                keys.push(k);
            }
        }
        out.push_str(&format!("{pad}{key}:\n{pad}  {}\n", keys.join("\t")));
        for row in rows {
            let fields: Vec<String> = keys.iter().map(|k| cell(&row[*k])).collect();
            out.push_str(&format!("{pad}  {}\n", fields.join("\t")));
        }
    } else if let Some(fields) = value.as_object() {
        out.push_str(&format!("{pad}{key}:\n"));
        let pad = format!("{pad}  ");
        fields.iter().for_each(|(k, v)| lines(k, v, &pad, out));
    } else {
        let text = cell(value).replace('\t', ", ");
        out.push_str(&format!("{pad}{key}: {text}\n"));
    }
}

/// The one text form: a list of rows is a tab-separated table, every other
/// field is `key: value`, and nested fields are indented.
pub fn render(data: &Map<String, Value>) -> String {
    let mut out = String::new();
    data.iter().for_each(|(k, v)| lines(k, v, "", &mut out));
    out
}

/// Prints an error: JSON on stdout with `--json`, always text on stderr; home as `~`.
pub fn print_error(err: &CommandError, json: bool) {
    let err = &redact_error(err);
    if json {
        stdout_line(&err.to_envelope().to_string());
    }
    let _ = writeln!(std::io::stderr().lock(), "{}", err.to_text());
}

/// clap's lines that go to the fix of a usage error.
const FIX_LINES: [&str; 3] = ["tip: ", "Usage: ", "[possible values"];

/// A clap parse error as a usage error. The what is clap's first paragraph
/// without `error: `, its list joined; the fix is its tips, values and usage.
pub(crate) fn usage_error(err: &clap::Error) -> CommandError {
    let rendered = err.to_string();
    let (mut what, mut fix, mut first) = (Vec::new(), Vec::new(), true);
    for line in rendered.trim_start().lines().map(str::trim) {
        first &= !line.is_empty();
        if FIX_LINES.iter().any(|p| line.starts_with(p)) {
            fix.push(line);
        } else if first {
            what.push(line.trim_start_matches("error: "));
        }
    }
    let what = match what.split_first() {
        Some((head, [])) => head.to_string(),
        Some((head, list)) => format!("{head} {}", list.join(", ")),
        None => "the command line could not be understood".to_owned(),
    };
    let fix = if fix.is_empty() {
        format!("Run `{BIN} commands` to list commands, or add --help.")
    } else if fix.iter().any(|line| line.starts_with("tip: ")) {
        fix.join(" ")
    } else {
        format!("{} Run `{BIN} --help` to list commands.", fix.join(" "))
    };
    let why = "the command line was not understood";
    CommandError::new(ErrorCode::Usage, what, why, fix)
}

/// Handles a clap "error", `--help` and `--version` included; returns the exit code.
pub fn handle_clap_error(err: &clap::Error, json: bool) -> u8 {
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            if json {
                let data = if err.kind() == ErrorKind::DisplayVersion {
                    json!({"name": BIN, "version": env!("CARGO_PKG_VERSION")})
                } else {
                    json!({ "help": err.to_string() })
                };
                stdout_line(&ok_envelope(data.as_object().unwrap()).to_string());
            } else {
                let _ = err.print();
            }
            0
        }
        ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand if !json => {
            // No command given: clap's own help on stderr is the clearest answer.
            let _ = err.print();
            2
        }
        _ => {
            let usage = usage_error(err);
            print_error(&usage, json);
            usage.exit_code().code()
        }
    }
}
