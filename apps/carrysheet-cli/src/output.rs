//! Printing results and errors. With `--json`, stdout holds one JSON
//! document and nothing else; messages for people go to stderr. No colour or
//! escape codes are ever written by this module.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use carrysheet_commands::paths::redact_home;
use carrysheet_commands::{CommandError, ErrorCode};
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
fn clap_message(rendered: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_keeps_lines_short() {
        let line = format!("  {:<13} {}", "overview", "word ".repeat(30));
        let wrapped = wrap(&line, 80, 16);
        for l in wrapped.lines() {
            assert!(l.chars().count() <= 80, "{l}");
        }
        assert!(wrapped.lines().nth(1).unwrap().starts_with(&" ".repeat(16)));
        assert_eq!(wrap("short", 80, 4), "short");
        assert_eq!(wrap("  a   b", 80, 4), "  a   b");
        assert_eq!(wrap("  aaa bbb", 6, 4), "  aaa\n    bbb");
    }

    #[test]
    fn usage_errors_carry_the_suggestion() {
        let err = crate::cli::build()
            .try_get_matches_from([BIN, "commands", "--jsno"])
            .unwrap_err();
        let e = usage_error(&err);
        assert_eq!(e.code, ErrorCode::Usage);
        assert!(e.what.contains("--jsno"), "{}", e.what);
        assert!(e.fix.contains("--json"), "{}", e.fix);
        assert_eq!(e.exit_code().code(), 2);
        assert!(!e.what.contains('\x1b') && !e.fix.contains('\x1b'));
    }

    fn usage_of(args: &[&str]) -> CommandError {
        let err = crate::cli::build().try_get_matches_from(args).unwrap_err();
        usage_error(&err)
    }

    #[test]
    fn missing_arguments_are_named() {
        let e = usage_of(&[BIN, "edit"]);
        assert!(
            e.what.ends_with("not provided: --ops <JSON>, <PATH>"),
            "{}",
            e.what
        );
        assert!(
            e.fix.starts_with("Give --ops <JSON> and <PATH>."),
            "{}",
            e.fix
        );
        assert!(e.fix.contains("Usage: carrysheet-cli edit"), "{}", e.fix);
        let e = usage_of(&[BIN, "mcp", "install"]);
        assert!(e.what.contains("<CLIENT>"), "{}", e.what);
    }

    #[test]
    fn bad_values_list_the_possible_ones() {
        let e = usage_of(&[BIN, "read", "a.xlsx", "--mode", "x"]);
        assert_eq!(e.what, "invalid value 'x' for '--mode <mode>'");
        assert!(
            e.fix.starts_with("Use one of: values, formulas, both."),
            "{}",
            e.fix
        );
        let e = usage_of(&[BIN, "mcp", "install", "notepad"]);
        assert!(e.fix.contains("claude-code, claude-desktop"), "{}", e.fix);
        let e = usage_of(&[BIN, "read", "a.xlsx", "--mode", "bo"]);
        assert!(e.fix.contains("'both'"), "{}", e.fix);
    }

    #[test]
    fn json_with_a_value_is_refused_with_a_fix() {
        for form in ["--json=true", "--json=false", "--json="] {
            let e = usage_of(&[BIN, "commands", form]);
            assert_eq!(e.code, ErrorCode::Usage, "{form}");
            assert!(
                e.fix.starts_with("Write --json without a value"),
                "{form}: {}",
                e.fix
            );
        }
    }

    #[test]
    fn clap_messages_keep_their_lists() {
        assert_eq!(
            clap_message("error: a list:\n  <A>\n  --b <B>\n\nUsage: x\n").as_deref(),
            Some("a list: <A>, --b <B>")
        );
        assert_eq!(
            clap_message("error: bad 'x'\n  [possible values: a, b]\n\n  tip: hm\n").as_deref(),
            Some("bad 'x'")
        );
        assert_eq!(clap_message("\n\n"), None);
    }

    #[test]
    fn unknown_commands_get_a_suggestion() {
        let err = crate::cli::build()
            .try_get_matches_from([BIN, "overveiw"])
            .unwrap_err();
        let e = usage_error(&err);
        assert!(e.fix.contains("overview"), "{}", e.fix);
    }

    #[test]
    fn ok_envelope_puts_ok_first() {
        let mut data = Map::new();
        data.insert("x".into(), Value::from(1));
        let v = ok_envelope(&data).to_string();
        assert_eq!(v, r#"{"ok":true,"x":1}"#);
    }
}
