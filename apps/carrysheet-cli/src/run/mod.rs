//! Running a command once its arguments are validated.

use std::path::Path;

use carrysheet::commands::paths::display_path;
use carrysheet::commands::schema::command_json;
use carrysheet::commands::table::{COMMANDS, Command};
use carrysheet::commands::{CommandError, ErrorCode, check_confirmation};
use carrysheet::{Kernel, run_within};
use serde_json::{Map, Value, json};

/// How the program was started. Nobody is ever asked a question: a
/// confirmation comes from `--yes` or the command's own arguments.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Context {
    /// `--yes`: gives `confirm` and `confirm_loss`.
    pub yes: bool,
    /// `--max-seconds`: the time one command may take; `None` is the
    /// default for agents (`limits::MAX_CALL_SECONDS`).
    pub max_seconds: Option<f64>,
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
            check_input_exists(command, args)?;
            let mut args = args.clone();
            for name in ["confirm", "confirm_loss"] {
                if ctx.yes && command.arg(name).is_some() {
                    args.insert(name.into(), Value::Bool(true));
                }
            }
            check_confirmation(command, &args)?;
            let data = run_within(&Kernel::new(), command, &args, ctx.max_seconds)?;
            Ok(Done { data, plain: None })
        }
    }
}

/// Fails early with a clear error when the input workbook is missing.
fn check_input_exists(command: &Command, args: &Map<String, Value>) -> Result<(), CommandError> {
    if command.arg("path").is_none() {
        return Ok(());
    }
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
        .map(|c| json!({ "command": c.cli_name(), "summary": c.summary }))
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
