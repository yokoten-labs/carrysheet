//! The command line. clap's derive on each command's argument struct
//! (`carrysheet::args`) makes its flags and help.

use std::path::Path;

use carrysheet::args::{COMMANDS, Command, PATHS, command, input_schema};
use carrysheet::commands::{CommandError, ErrorCode};
use carrysheet::open::ReadFailure;
use clap::{ArgAction, ArgMatches};
use serde_json::{Map, Value, json};

/// The program name, as typed.
pub const BIN: &str = "carrysheet-cli";

/// The largest `@file` an argument may be read from.
const MAX_ARG_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// One command as clap sees it: `name` is its last word, such as `serve`.
fn clap_command(name: &'static str, command: &Command) -> clap::Command {
    let (summary, description) = command.doc();
    (command.flags)(clap::Command::new(name))
        .about(summary)
        .long_about(description)
}

/// Builds the whole command line from [`COMMANDS`].
pub fn build() -> clap::Command {
    let mut root = clap::Command::new(BIN)
        .version(env!("CARGO_PKG_VERSION"))
        .about("Carrysheet: read, convert and edit spreadsheets, for people and AI agents")
        .after_help(
            "Agents: run `carrysheet-cli commands --json` for every command and its schema.",
        )
        .max_term_width(80)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            clap::Arg::new("json")
                .long("json")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Print only JSON on stdout, errors included"),
        )
        .arg(
            clap::Arg::new("yes")
                .long("yes")
                .short('y')
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Confirm deleting, overwriting and saves that lose content (--confirm and --confirm-loss)"),
        )
        .arg(
            clap::Arg::new("max-seconds")
                .long("max-seconds")
                .global(true)
                .value_name("SECONDS")
                .value_parser(clap::value_parser!(f64))
                .help("Most seconds one command may take, opening and saving included (default 10, at most 3600)"),
        );
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

/// The single-type schema of an argument, without its null branch.
fn kind(property: &Value) -> &Value {
    property.pointer("/anyOf/0").unwrap_or(property)
}

/// Everything about a command, for `commands --json`.
pub fn command_json(command: &Command) -> Value {
    let schema = input_schema(command);
    let (summary, description) = command.doc();
    let args: Vec<Value> = clap_command("", command)
        .get_arguments()
        .map(|arg| {
            let name = arg.get_id().as_str();
            let property = &schema["properties"][name];
            let type_name = match kind(property)["type"].as_str() {
                Some("string") if kind(property).get("enum").is_some() => "enum",
                Some("string") if PATHS.contains(&name) => "path",
                Some("string") if name == "range" => "range",
                Some("string") => "text",
                Some("array") => "object_list",
                Some(other) => other,
                None => "value",
            };
            json!({
                "name": name,
                "flag": arg.get_long().map(|long| format!("--{long}")),
                "positional": arg.is_positional(),
                "required": arg.is_required_set(),
                "type": type_name,
                "description": property["description"],
            })
        })
        .collect();
    json!({
        "name": command.name,
        "cli": command.cli_name(),
        "summary": summary,
        "description": description,
        "changes_files": command.changes_files(),
        "mcp_tool": command.mcp_tool,
        "annotations": command.annotations(),
        "args": args,
        "input_schema": schema,
    })
}

/// Reads an `@file` argument value, or returns the text as it is.
fn read_at_file(name: &str, text: &str) -> Result<String, CommandError> {
    let Some(file) = text.strip_prefix('@') else {
        return Ok(text.to_owned());
    };
    let path = Path::new(file);
    let shown = crate::files::display_path(path);
    let flag = format!("--{}", name.replace('_', "-"));
    let what = format!("cannot read {flag} from {shown}");
    let bytes = crate::files::read_bounded(path, MAX_ARG_FILE_BYTES).map_err(|e| match e {
        ReadFailure::Io(e) if e.kind() == std::io::ErrorKind::NotFound => CommandError::new(
            ErrorCode::FileNotFound,
            &what,
            e.to_string(),
            format!("Check the file path after @, or pass the JSON itself to {flag}."),
        ),
        ReadFailure::Io(e) => CommandError::new(
            ErrorCode::Io,
            &what,
            e.to_string(),
            "Check that the file exists and you can read it.",
        ),
        ReadFailure::NotAFile => CommandError::invalid_argument(
            name,
            format!("{shown} is not a regular file (a folder, device or pipe)"),
            format!("Save the JSON to a file and pass {flag} @file.json, or pass the JSON itself."),
        ),
        ReadFailure::TooLarge => CommandError::invalid_argument(
            name,
            format!("{shown} is larger than 16 MB"),
            "Split the operations into several files.",
        ),
    })?;
    String::from_utf8(bytes).map_err(|_| {
        CommandError::new(
            ErrorCode::InvalidArgument,
            what,
            "the file is not UTF-8 text",
            "Save the file as UTF-8 and try again.",
        )
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
            (_, Some(text)) => {
                let text = read_at_file(name, text)?;
                serde_json::from_str(&text).map_err(|e| {
                    let flag = name.replace('_', "-");
                    CommandError::invalid_argument(
                        name,
                        format!("--{flag} is not valid JSON: {e}"),
                        format!(
                            "Pass a JSON list, or @file.json. Example: --{flag} '[{{\"op\":\"set_value\",\"target\":\"A1\",\"value\":3}}]'"
                        ),
                    )
                })?
            }
        };
        out.insert(name.clone(), value);
    }
    Ok(out)
}
