//! Printing results and errors. With `--json`, stdout holds one JSON
//! document and nothing else; messages for people go to stderr.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use carrysheet::commands::paths::redact_home;
use carrysheet::commands::{CommandError, ErrorCode};
use carrysheet::save::clean_from_file;
use clap::error::ErrorKind;
use serde_json::{Map, Value, json};

use crate::cli::BIN;
use crate::run::Done;

/// Set while `mcp serve` runs: stdout then belongs to the MCP transport.
static STDOUT_RESERVED: AtomicBool = AtomicBool::new(false);

/// Gives stdout to the MCP transport; [`stdout_line`] then writes to stderr.
pub fn reserve_stdout_for_mcp() {
    STDOUT_RESERVED.store(true, Ordering::SeqCst);
}

/// The only writer to stdout (clippy's `disallowed-methods` keeps it so);
/// a closed pipe (`| head`) is ignored.
#[expect(clippy::disallowed_methods, reason = "the one guarded writer")]
fn stdout_line(text: &str) {
    if STDOUT_RESERVED.load(Ordering::SeqCst) {
        return stderr_line(text);
    }
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

fn stderr_line(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
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
        Value::String(s) => clean_from_file(&redact_home(s), 2000),
        Value::Number(n) if !n.is_f64() => n.to_string(),
        Value::Number(n) => n.as_f64().unwrap_or_default().to_string(),
        other => clean_from_file(&redact_home(&other.to_string()), 2000),
    }
}

fn is_flat(list: &[Value]) -> bool {
    list.iter().all(|v| !v.is_object() && !v.is_array())
}

/// One table field; a list of plain values spreads over several fields.
fn cell(value: &Value) -> String {
    match value.as_array() {
        Some(list) if is_flat(list) => list.iter().map(scalar).collect::<Vec<_>>().join("\t"),
        _ => scalar(value),
    }
}

fn lines(key: &str, value: &Value, pad: &str, out: &mut String) {
    let list = value.as_array().filter(|l| !l.is_empty());
    let rows = list.filter(|l| l.iter().all(Value::is_object));
    if let Some(rows) = rows {
        let mut keys: Vec<&String> = Vec::new();
        for k in rows.iter().flat_map(|r| r.as_object().unwrap().keys()) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        out.push_str(&format!("{pad}{key}:\n{pad}  "));
        let names: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
        out.push_str(&names.join("\t"));
        for row in rows {
            out.push_str(&format!("\n{pad}  "));
            let fields: Vec<String> = keys.iter().map(|k| cell(&row[k.as_str()])).collect();
            out.push_str(&fields.join("\t"));
        }
        out.push('\n');
    } else if let Some(fields) = value.as_object() {
        out.push_str(&format!("{pad}{key}:\n"));
        fields
            .iter()
            .for_each(|(k, v)| lines(k, v, &format!("{pad}  "), out));
    } else {
        out.push_str(&format!(
            "{pad}{key}: {}\n",
            cell(value).replace('\t', ", ")
        ));
    }
}

/// The one text form: a list of rows is a tab-separated table, every other
/// field is `key: value`, and nested fields are indented.
pub fn render(data: &Map<String, Value>) -> String {
    let mut out = String::new();
    data.iter().for_each(|(k, v)| lines(k, v, "", &mut out));
    out
}

/// Prints an error: JSON on stdout with `--json`, and always text on stderr.
/// Both forms show the home folder as `~`.
pub fn print_error(err: &CommandError, json: bool) {
    if json {
        stdout_line(&err.to_envelope().to_string());
    }
    stderr_line(&err.to_text());
}

/// The message part of clap's rendered error: every line before the first
/// blank one, without the `error:` prefix and the list of possible values
/// (which goes to the fix). Lists such as the missing arguments are joined.
pub(crate) fn clap_message(rendered: &str) -> Option<String> {
    let mut lines = rendered
        .lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty())
        .take_while(|l| !l.is_empty())
        .filter(|l| !l.starts_with("[possible values"));
    let first = lines.next()?;
    let first = first.strip_prefix("error: ").unwrap_or(first);
    let rest: Vec<&str> = lines.collect();
    Some(if rest.is_empty() {
        first.to_owned()
    } else {
        format!("{first} {}", rest.join(", "))
    })
}

/// Turns a clap parse error into a usage error: clap's own message is the
/// what, and its tips, possible values and usage line are the fix.
pub fn usage_error(err: &clap::Error) -> CommandError {
    let rendered = err.to_string();
    let what = clap_message(&rendered)
        .unwrap_or_else(|| "the command line could not be understood".to_owned());
    let help: Vec<&str> = rendered
        .lines()
        .map(str::trim)
        .filter(|l| {
            ["tip: ", "Usage: ", "[possible values"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .collect();
    let fix = if help.is_empty() {
        format!("Run `{BIN} commands` to list commands, or add --help.")
    } else {
        help.join(" ")
    };
    CommandError::new(
        ErrorCode::Usage,
        what,
        "the command line was not understood",
        fix,
    )
}

/// Handles a clap "error", which includes `--help` and `--version`.
/// Returns the exit code.
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
