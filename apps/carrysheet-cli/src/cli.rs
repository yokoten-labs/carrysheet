//! The command line. clap's derive on each command's argument struct
//! (`carrysheet::args`) makes its flags and help.

use std::path::Path;

use carrysheet::args::{COMMANDS, Command, OPERATION_EXAMPLE, command, input_schema};
use carrysheet::commands::limits::MAX_CLI_SECONDS;
use carrysheet::commands::{CommandError, ErrorCode};
use carrysheet::open::ReadFailure;
use clap::{ArgMatches, Args};
use serde_json::{Map, Value};

/// The program name, as typed.
pub const BIN: &str = "carrysheet-cli";

/// The largest `@file` an argument may be read from.
const MAX_ARG_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The flags every command takes.
#[derive(Args)]
pub struct Global {
    /// Print only JSON on stdout, errors included
    #[arg(long, global = true)]
    pub json: bool,
    /// Confirm deleting, overwriting and saves that lose content (--confirm and --confirm-loss)
    #[arg(long, short, global = true)]
    pub yes: bool,
    /// Most seconds one command may take, opening and saving included (default 10, at most 3600)
    #[arg(long, global = true, value_name = "SECONDS")]
    pub max_seconds: Option<f64>,
}

/// One command as clap sees it: `name` is its last word, such as `serve`.
pub(crate) fn clap_command(name: &'static str, command: &Command) -> clap::Command {
    let (summary, description) = command.doc();
    (command.flags)(clap::Command::new(name))
        .about(summary)
        .long_about(description)
}

/// Builds the whole command line from [`COMMANDS`].
pub fn build() -> clap::Command {
    let mut root = Global::augment_args(clap::Command::new(BIN))
        .version(env!("CARGO_PKG_VERSION"))
        .about("Carrysheet: read, convert and edit spreadsheets, for people and AI agents")
        .after_help(
            "Agents: run `carrysheet-cli commands --json` for every command and its schema.",
        )
        .max_term_width(80)
        .subcommand_required(true)
        .arg_required_else_help(true);
    // A name of two words, such as `mcp_serve`, is typed `mcp serve`.
    let mut mcp = clap::Command::new("mcp")
        .about("Run the MCP server for AI agents")
        .subcommand_required(true)
        .arg_required_else_help(true);
    for command in COMMANDS {
        match command.name.strip_prefix("mcp_") {
            Some(name) => mcp = mcp.subcommand(clap_command(name, command)),
            None => root = root.subcommand(clap_command(command.name, command)),
        }
    }
    root.subcommand(mcp)
}

/// The command a parse chose, and the matches holding its arguments.
pub fn chosen(matches: &ArgMatches) -> Option<(&'static Command, &ArgMatches)> {
    let (first, sub) = matches.subcommand()?;
    match sub.subcommand() {
        Some((second, sub)) => Some((command(&format!("{first}_{second}"))?, sub)),
        None => Some((command(first)?, sub)),
    }
}

/// `--max-seconds`, checked: more than 0 and at most an hour.
pub fn max_seconds(global: &Global) -> Result<Option<f64>, CommandError> {
    let max = MAX_CLI_SECONDS;
    match global.max_seconds {
        Some(seconds) if !(seconds > 0.0 && seconds <= max) => Err(CommandError::invalid_argument(
            "max-seconds",
            format!("--max-seconds must be more than 0 and at most {max}"),
            "Pass a number of seconds, like --max-seconds 60.",
        )),
        seconds => Ok(seconds),
    }
}

/// The single-type schema of an argument, without its null branch.
pub(crate) fn kind(property: &Value) -> &Value {
    property.pointer("/anyOf/0").unwrap_or(property)
}

/// An argument's JSON text, or the text of the `@file` it names, parsed.
fn parse_json_arg(name: &str, text: &str) -> Result<Value, CommandError> {
    let flag = format!("--{}", name.replace('_', "-"));
    let text = match text.strip_prefix('@') {
        None => text.to_owned(),
        Some(file) => {
            let path = Path::new(file);
            let shown = crate::files::display_path(path);
            let what = format!("cannot read {flag} from {shown}");
            let bytes = crate::files::read_bounded(path, MAX_ARG_FILE_BYTES).map_err(|e| match e {
                ReadFailure::Io(e) => {
                    let is_missing = e.kind() == std::io::ErrorKind::NotFound;
                    let code = if is_missing { ErrorCode::FileNotFound } else { ErrorCode::Io };
                    let fix = format!("Check the file path after @ and that you can read it, or pass the JSON itself to {flag}.");
                    CommandError::new(code, &what, e.to_string(), fix)
                }
                ReadFailure::NotAFile => {
                    let why = format!("{shown} is not a regular file (a folder, device or pipe)");
                    let fix = format!("Save the JSON to a file and pass {flag} @file.json, or pass the JSON itself.");
                    CommandError::invalid_argument(name, why, fix)
                }
                ReadFailure::TooLarge => {
                    let why = format!("{shown} is larger than 16 MB");
                    CommandError::invalid_argument(name, why, "Split the operations into several files.")
                }
            })?;
            String::from_utf8(bytes).map_err(|_| {
                let why = "the file is not UTF-8 text";
                let fix = "Save the file as UTF-8 and try again.";
                CommandError::new(ErrorCode::InvalidArgument, &what, why, fix)
            })?
        }
    };
    serde_json::from_str(&text).map_err(|e| {
        let fix =
            format!("Pass a JSON list, or @file.json. Example: {flag} '[{OPERATION_EXAMPLE}]'");
        CommandError::invalid_argument(name, format!("{flag} is not valid JSON: {e}"), fix)
    })
}

/// Turns parsed flags into the JSON object an MCP client would send. The
/// flags are read as text, so `validate_args` checks both the same way.
pub fn args_json(command: &Command, m: &ArgMatches) -> Result<Map<String, Value>, CommandError> {
    let schema = input_schema(command);
    let mut out = Map::new();
    for (name, property) in schema["properties"].as_object().into_iter().flatten() {
        let kind = kind(property)["type"].as_str();
        let text = match kind {
            Some("boolean") => m.get_flag(name).then_some("true"),
            _ => m.get_raw(name).and_then(|mut v| v.next()?.to_str()),
        };
        let value = match (kind, text) {
            (_, None) => Value::Null,
            (Some("string"), Some(text)) => Value::String(text.to_owned()),
            // A whole number, a flag, or JSON text for a list (or @file).
            (_, Some(text)) => parse_json_arg(name, text)?,
        };
        out.insert(name.clone(), value);
    }
    Ok(out)
}
