//! Printing results and errors. With `--json`, stdout holds one JSON
//! document and nothing else; messages for people go to stderr. No colour or
//! escape codes are ever written by this module.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use carrysheet::commands::paths::redact_home;
use carrysheet::commands::{CommandError, ErrorCode};
use clap::error::{ContextKind, ContextValue, ErrorKind};
use serde_json::{Map, Value};

use crate::cli::BIN;
use crate::run::Done;

/// Word-wraps one line to `width` characters, indenting continuation lines
/// by `indent` spaces. Words longer than a line are left whole.
pub fn wrap(line: &str, width: usize, indent: usize) -> String {
    let body = line.trim_start_matches(' ');
    let mut out = String::new();
    let mut current = " ".repeat(line.len() - body.len());
    let mut at_start = true;
    for word in body.split(' ') {
        let len = current.chars().count();
        if !at_start && len + 1 + word.chars().count() > width && !word.is_empty() {
            out.push_str(current.trim_end());
            out.push('\n');
            current = " ".repeat(indent);
        } else if !at_start {
            current.push(' ');
        }
        current.push_str(word);
        at_start = false;
    }
    out.push_str(current.trim_end());
    out
}

/// Set while `mcp serve` runs: stdout then belongs to the MCP transport.
static STDOUT_RESERVED: AtomicBool = AtomicBool::new(false);

/// Gives stdout to the MCP transport for the rest of the process. From now
/// on [`stdout_line`] writes to stderr instead, so a stray message can never
/// corrupt the JSON-RPC stream.
pub fn reserve_stdout_for_mcp() {
    STDOUT_RESERVED.store(true, Ordering::SeqCst);
}

/// Prints one line to stdout, ignoring a closed pipe (`| head`). This is the
/// only place the program writes to stdout (clippy's `disallowed-methods`
/// keeps it that way), apart from clap's own help and the MCP transport.
#[expect(
    clippy::disallowed_methods,
    reason = "the one guarded writer to stdout"
)]
fn stdout_line(text: &str) {
    if STDOUT_RESERVED.load(Ordering::SeqCst) {
        stderr_line(text);
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn stderr_line(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

/// The success envelope: `{"ok": true, ...data}`.
pub fn ok_envelope(data: &Map<String, Value>) -> Value {
    let mut map = Map::new();
    map.insert("ok".into(), Value::Bool(true));
    for (k, v) in data {
        map.insert(k.clone(), v.clone());
    }
    Value::Object(map)
}

/// Prints a result.
pub fn print_done(done: &Done, json: bool) {
    if json {
        stdout_line(&ok_envelope(&done.data).to_string());
    } else {
        let text = done.text.trim_end_matches('\n');
        stdout_line(text);
    }
}

/// Prints an error: JSON on stdout with `--json`, and always text on stderr.
/// Both forms show the home folder as `~` (see [`CommandError::to_json`]).
pub fn print_error(err: &CommandError, json: bool) {
    if json {
        stdout_line(&err.to_envelope().to_string());
    }
    stderr_line(&err.to_text());
}

/// The text of a clap error context value, such as the allowed values.
fn context_strings(err: &clap::Error, kind: ContextKind) -> Vec<String> {
    match err.get(kind) {
        Some(ContextValue::String(s)) => vec![s.clone()],
        Some(ContextValue::Strings(list)) => list.clone(),
        _ => Vec::new(),
    }
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

/// Turns a clap parse error into a usage error with what, why and fix.
pub fn usage_error(err: &clap::Error) -> CommandError {
    let rendered = err.to_string();
    let what = clap_message(&rendered)
        .unwrap_or_else(|| "the command line could not be understood".to_owned());
    let mut tips = Vec::new();
    let mut usage = None;
    for line in rendered.lines().map(str::trim) {
        if let Some(tip) = line.strip_prefix("tip: ") {
            tips.push(tip.to_owned());
        } else if let Some(u) = line.strip_prefix("Usage: ") {
            usage = Some(u.to_owned());
        }
    }
    let why = match err.kind() {
        ErrorKind::UnknownArgument => "this command has no such option",
        ErrorKind::InvalidSubcommand => "there is no command with that name",
        ErrorKind::MissingRequiredArgument => "a required argument is missing",
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            "a command is required"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
            "the value is not one this option accepts"
        }
        ErrorKind::ArgumentConflict => "two options cannot be used together",
        ErrorKind::TooManyValues | ErrorKind::WrongNumberOfValues | ErrorKind::TooFewValues => {
            "an option got the wrong number of values"
        }
        ErrorKind::InvalidUtf8 => "an argument is not valid UTF-8 text",
        _ => "the command line could not be understood",
    };
    let mut fix = String::new();
    let invalid = context_strings(err, ContextKind::InvalidArg);
    if err.kind() == ErrorKind::TooManyValues && invalid.iter().any(|a| a == "--json") {
        fix.push_str("Write --json without a value, like `carrysheet-cli commands --json`. ");
    }
    if err.kind() == ErrorKind::MissingRequiredArgument && !invalid.is_empty() {
        fix.push_str(&format!("Give {}. ", invalid.join(" and ")));
    }
    let valid = context_strings(err, ContextKind::ValidValue);
    if !valid.is_empty() {
        fix.push_str(&format!("Use one of: {}. ", valid.join(", ")));
    }
    for tip in &tips {
        fix.push_str(&capitalize(tip));
        fix.push_str(". ");
    }
    match usage {
        Some(u) => fix.push_str(&format!("Usage: {u}")),
        None => fix.push_str(&format!(
            "Run `{BIN} commands` to list commands, or add --help."
        )),
    }
    CommandError::new(ErrorCode::Usage, what, why, fix.trim_end())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Handles a clap "error", which includes `--help` and `--version`.
/// Returns the exit code.
pub fn handle_clap_error(err: &clap::Error, json: bool) -> u8 {
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            if json {
                let mut data = Map::new();
                if err.kind() == ErrorKind::DisplayVersion {
                    data.insert("name".into(), Value::String(BIN.into()));
                    data.insert(
                        "version".into(),
                        Value::String(env!("CARGO_PKG_VERSION").into()),
                    );
                } else {
                    data.insert("help".into(), Value::String(err.to_string()));
                }
                stdout_line(&ok_envelope(&data).to_string());
            } else {
                let _ = err.print();
            }
            0
        }
        _ => {
            let usage = usage_error(err);
            if json {
                print_error(&usage, true);
            } else {
                // clap repeats what was typed; hide the home folder in it.
                stderr_line(redact_home(&err.to_string()).trim_end());
            }
            usage.exit_code().code()
        }
    }
}
