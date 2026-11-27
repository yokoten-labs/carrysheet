//! Running a command once its arguments are validated.

use std::path::Path;

use carrysheet_commands::engine::run::run_within;
use carrysheet_commands::paths::display_path;
use carrysheet_commands::schema::command_json;
use carrysheet_commands::table::{COMMANDS, Command};
use carrysheet_commands::{CommandError, ErrorCode, check_confirmation};
use carrysheet_kernel::Kernel;
use serde_json::{Map, Value, json};

use crate::output::wrap;

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

/// What a command produced: JSON fields for `--json` and text for people.
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    /// Fields placed beside `"ok": true`.
    pub data: Map<String, Value>,
    /// What a person sees on stdout.
    pub text: String,
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
            let text = text_for(command.name, &data);
            Ok(Done { data, text })
        }
    }
}

/// A value from a result as text: numbers without a trailing `.0`,
/// booleans as JSON writes them, text as it is, empty as nothing.
fn plain(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), |f| f.to_string()),
        Value::Object(o) => match (o.get("formula"), o.get("value")) {
            (Some(Value::String(f)), _) => f.clone(),
            (_, Some(v)) => plain(v),
            _ => v.to_string(),
        },
        other => other.to_string(),
    }
}

fn get_str<'a>(data: &'a Map<String, Value>, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or("")
}

fn list<'a>(data: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    data.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// What a person sees for a workbook command.
fn text_for(name: &str, data: &Map<String, Value>) -> String {
    let mut t = String::new();
    match name {
        "overview" => {
            t.push_str(&format!(
                "{} ({} date system, {} defined names)\n",
                get_str(data, "file"),
                get_str(data, "date_system"),
                data.get("defined_names").unwrap_or(&Value::Null)
            ));
            if let Some(macros) = data.get("macros").and_then(Value::as_str) {
                t.push_str(&wrap(&format!("  Macros: {macros}."), 80, 2));
                t.push('\n');
            }
            for s in list(data, "sheets") {
                let used = s["used_range"].as_str().unwrap_or("empty");
                t.push_str(&format!(
                    "  {}  {used}  {} cells, {} formulas, {} errors\n",
                    plain(&s["name"]),
                    s["cells"],
                    s["formulas"],
                    s["errors"]
                ));
                if let Some(cells) = s["header"]["cells"].as_array() {
                    let header: Vec<String> = cells.iter().map(plain).collect();
                    let line = format!("    row {}: {}", s["header"]["row"], header.join(" | "));
                    t.push_str(&wrap(&line, 80, 6));
                    t.push('\n');
                }
            }
        }
        "read" => {
            let range = get_str(data, "range");
            t.push_str(&format!("{}!{}\n", get_str(data, "sheet"), range));
            let columns: Vec<String> = list(data, "columns").iter().map(plain).collect();
            if !columns.is_empty() {
                t.push_str(&format!("\t{}\n", columns.join("\t")));
            }
            for row in list(data, "rows") {
                let cells: Vec<String> = row["cells"]
                    .as_array()
                    .map(|c| c.iter().map(plain).collect())
                    .unwrap_or_default();
                t.push_str(&format!("{}\t{}\n", row["row"], cells.join("\t")));
            }
            if let Some(cursor) = data.get("next_cursor").and_then(Value::as_str) {
                t.push_str(&format!("More cells remain: add --cursor \"{cursor}\"\n"));
            }
        }
        "eval" => {
            t.push_str(&format!(
                "{}\n",
                plain(data.get("value").unwrap_or(&Value::Null))
            ));
        }
        _ => {
            let output = get_str(data, "output");
            if data.get("written") == Some(&Value::Bool(true)) {
                t.push_str(&format!("Wrote {output} ({} bytes)\n", data["bytes"]));
            } else {
                t.push_str(&format!("Dry run: nothing written; would write {output}\n"));
            }
            for op in list(data, "ops") {
                t.push_str(&format!(
                    "  {} {}: {}\n",
                    plain(&op["op"]),
                    plain(&op["target"]),
                    plain(&op["done"])
                ));
            }
            for (key, label) in [("changes", "changed"), ("recalculated", "recalculated")] {
                let total = data
                    .get(&format!("{key}_total"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if total == 0 {
                    continue;
                }
                t.push_str(&format!("{total} cell(s) {label}:\n"));
                for c in list(data, key).iter().take(20) {
                    t.push_str(&format!(
                        "  {}!{}: {} -> {}\n",
                        plain(&c["sheet"]),
                        plain(&c["cell"]),
                        plain(&c["before"]),
                        plain(&c["after"])
                    ));
                }
                if total > 20 {
                    t.push_str(&format!("  ... and {} more (see --json)\n", total - 20));
                }
            }
            let save = data.get("save").cloned().unwrap_or(Value::Null);
            let lost = save["lost"].as_array().cloned().unwrap_or_default();
            if lost.is_empty() && save["dropped_parts"].as_array().is_none_or(Vec::is_empty) {
                t.push_str("Nothing known is lost.\n");
            } else {
                t.push_str("Lost in this save:\n");
                for l in &lost {
                    t.push_str(&format!(
                        "  {} ({} of {})\n",
                        plain(&l["feature"]),
                        l["before"]
                            .as_u64()
                            .unwrap_or(0)
                            .saturating_sub(l["after"].as_u64().unwrap_or(0)),
                        l["before"]
                    ));
                }
                for p in save["dropped_parts"].as_array().into_iter().flatten() {
                    t.push_str(&format!("  part {}\n", plain(p)));
                }
            }
            for n in save["notes"].as_array().into_iter().flatten() {
                t.push_str(&format!("Note: {}\n", plain(n)));
            }
        }
    }
    t
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
    let mut text = String::new();
    for c in COMMANDS {
        let line = format!("  {:<13} {}", c.cli_name(), c.summary);
        text.push_str(&wrap(&line, 80, 16));
        text.push('\n');
    }
    text.push_str(
        "\nRun `carrysheet-cli <command> --help` for details, or add --json for schemas.\n",
    );
    let mut data = Map::new();
    data.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    data.insert("commands".into(), Value::Array(list));
    Done { data, text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use carrysheet_commands::{command, validate_args};

    fn ctx() -> Context {
        Context::default()
    }

    fn run_json(name: &str, args: Value) -> Result<Done, CommandError> {
        let c = command(name).unwrap();
        let args = validate_args(c, &args)?;
        run(c, &args, &ctx())
    }

    #[test]
    fn deleting_needs_confirm_or_yes() {
        let args = json!({
            "path": "Cargo.toml",
            "ops": [{ "op": "delete_sheet", "target": "Old" }]
        });
        let e = run_json("edit", args.clone()).unwrap_err();
        assert_eq!(e.code, ErrorCode::ConfirmationRequired);
        assert_eq!(e.exit_code().code(), 1);

        let mut with = args.clone();
        with["confirm"] = json!(true);
        let e = run_json("edit", with).unwrap_err();
        assert_eq!(
            e.code,
            ErrorCode::UnsupportedFormat,
            "confirmed, then the file is looked at"
        );

        let mut dry = args.clone();
        dry["dry_run"] = json!(true);
        assert_eq!(
            run_json("edit", dry).unwrap_err().code,
            ErrorCode::UnsupportedFormat
        );

        let c = command("edit").unwrap();
        let checked = validate_args(c, &args).unwrap();
        let yes = Context { yes: true, ..ctx() };
        assert_eq!(
            run(c, &checked, &yes).unwrap_err().code,
            ErrorCode::UnsupportedFormat
        );
    }

    #[test]
    fn missing_input_is_reported_first() {
        let e = run_json("overview", json!({ "path": "no-such-book.xlsx" })).unwrap_err();
        assert_eq!(e.code, ErrorCode::FileNotFound);
        assert!(e.what.contains("no-such-book.xlsx"));
    }

    #[test]
    fn commands_lists_everything() {
        let done = run_json("commands", json!({})).unwrap();
        assert_eq!(
            done.data["commands"].as_array().unwrap().len(),
            COMMANDS.len()
        );
        assert_eq!(done.data["commands"][0]["implemented"], true);
        for line in done.text.lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
    }
}
