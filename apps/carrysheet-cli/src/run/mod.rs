//! Running a command once its arguments are validated.

use std::path::{Path, PathBuf};

use carrysheet::args::{PATHS, input_schema};
use carrysheet::commands::{COMMANDS, Command, CommandError, ErrorCode, check_confirmation};
use carrysheet::open::{MAX_FILE_BYTES, check_opens};
use carrysheet::save::prepare_output;
use carrysheet::{Kernel, run_within};
use serde_json::{Map, Value, json};

use crate::cli::{clap_command, kind};
use crate::files::{display_path, exists, read_bounded, write_atomic};

/// How the program was started. Nobody is ever asked a question: a
/// confirmation comes from `--yes` or the command's own arguments.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    /// `--yes`: gives `confirm` and `confirm_loss`.
    pub yes: bool,
    /// `--max-seconds`: the time one command may take; `None` is the
    /// default for agents (`limits::MAX_CALL_SECONDS`).
    pub max_seconds: Option<f64>,
    /// `mcp serve`'s root folder: writes must stay inside it.
    pub root: Option<PathBuf>,
}

/// What a command produced: JSON fields, which people see as text too.
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    /// Fields placed beside `"ok": true`.
    pub data: Map<String, Value>,
    /// Fields shown to people instead of `data`, when `data` is too big to
    /// read (`commands`).
    pub plain: Option<Map<String, Value>>,
}

/// Runs a command with validated arguments.
pub fn run(
    command: &Command,
    args: &Map<String, Value>,
    ctx: &Context,
) -> Result<Done, CommandError> {
    match command.name {
        "commands" => Ok(commands()),
        // `main` hands `mcp serve` to `mcp::serve` before getting here.
        "mcp_serve" => Err(CommandError::new(
            ErrorCode::Internal,
            "`mcp serve` cannot run as a tool",
            "the MCP server is started by the command line, not from inside itself",
            "Start it with `carrysheet-cli mcp serve`.",
        )),
        _ => {
            check_input_exists(args)?;
            let mut args = args.clone();
            for name in ["confirm", "confirm_loss"] {
                if ctx.yes && args.contains_key(name) {
                    args.insert(name.into(), Value::Bool(true));
                }
            }
            check_confirmation(command, &args)?;
            let output = prepare_output(command, &mut args, &exists)?;
            let path = PathBuf::from(args.get("path").and_then(Value::as_str).unwrap_or(""));
            check_opens(&path)?;
            let input = read_bounded(&path, MAX_FILE_BYTES).map_err(|e| e.error(&path))?;
            let answer = run_within(&Kernel::new(), command, &args, input, ctx.max_seconds)?;
            let mut data = answer.data;
            if let (Some(bytes), Some(output)) = (answer.save, output) {
                write_atomic(&output, &bytes, ctx.root.as_deref())?;
                data.insert("written".into(), json!(true));
                data.insert("bytes".into(), json!(bytes.len()));
            }
            for name in ["file", "output"] {
                if let Some(Value::String(shown)) = data.get_mut(name) {
                    *shown = display_path(Path::new(shown));
                }
            }
            Ok(Done { data, plain: None })
        }
    }
}

/// Fails early with a clear error when the input workbook is missing.
fn check_input_exists(args: &Map<String, Value>) -> Result<(), CommandError> {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Ok(());
    };
    let p = Path::new(path);
    if p.exists() {
        return Ok(());
    }
    Err(CommandError::new(
        ErrorCode::FileNotFound,
        format!("{} does not exist", display_path(p)),
        "there is no file at that path",
        "Check the spelling, or give the full path to the workbook.",
    ))
}

fn commands() -> Done {
    let list: Vec<Value> = COMMANDS
        .iter()
        .map(|c| {
            let mut v = command_json(c);
            v["implemented"] = Value::Bool(true);
            v
        })
        .collect();
    let rows: Vec<Value> = COMMANDS
        .iter()
        .map(|c| json!({ "command": c.cli_name(), "summary": c.doc().0 }))
        .collect();
    let mut plain = Map::new();
    plain.insert("commands".into(), Value::Array(rows));
    let mut data = Map::new();
    data.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    data.insert("commands".into(), Value::Array(list));
    Done {
        data,
        plain: Some(plain),
    }
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
