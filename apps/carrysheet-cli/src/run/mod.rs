//! Running a command once its arguments are validated.

mod macros;
mod package;
mod text;
mod xml;

use std::path::Path;

use carrysheet_commands::engine::run::{ENGINE_COMMANDS, run_within};
use carrysheet_commands::paths::{display_path, redact_home};
use carrysheet_commands::schema::command_json;
use carrysheet_commands::table::{COMMANDS, Command};
use carrysheet_commands::{CommandError, ErrorCode, check_confirmation};
use carrysheet_kernel::Kernel;
use serde_json::{Map, Value, json};

use crate::output::wrap;
use crate::prompt::{Context, confirm};

/// What a command produced: JSON fields for `--json` and text for people.
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    /// Fields placed beside `"ok": true`.
    pub data: Map<String, Value>,
    /// What a person sees on stdout.
    pub text: String,
}

/// Commands that work today. Everything else in the table answers "not
/// implemented yet".
pub const IMPLEMENTED: &[&str] = &[
    "overview",
    "read",
    "eval",
    "convert",
    "edit",
    "feedback",
    "macros",
    "mcp_serve",
    "commands",
];

pub fn is_implemented(command: &Command) -> bool {
    IMPLEMENTED.contains(&command.name)
}

/// Runs a command with validated arguments.
pub fn run(
    command: &Command,
    args: &Map<String, Value>,
    ctx: &Context,
) -> Result<Done, CommandError> {
    match command.name {
        "commands" => Ok(commands()),
        "feedback" => feedback(args),
        "macros" => {
            check_input_exists(command, args)?;
            macros::macros(args)
        }
        // `main` hands `mcp serve` to `mcp::serve` before getting here.
        "mcp_serve" => Err(CommandError::new(
            ErrorCode::Internal,
            "`mcp serve` cannot run as a tool",
            "the MCP server is started by the command line, not from inside itself",
            "Start it with `carrysheet-cli mcp serve`.",
        )),
        _ => {
            check_input_exists(command, args)?;
            confirmed(command, args, ctx)?;
            if !ENGINE_COMMANDS.contains(&command.name) {
                return Err(CommandError::not_implemented(&command.cli_name()));
            }
            let data = on_kernel(command, args, ctx)?;
            let text = text_for(command.name, &data);
            Ok(Done { data, text })
        }
    }
}

/// Runs a workbook command on the kernel. `--yes` confirms a save that
/// loses something; at a terminal a person is asked.
fn on_kernel(
    command: &Command,
    args: &Map<String, Value>,
    ctx: &Context,
) -> Result<Map<String, Value>, CommandError> {
    let kernel = Kernel::new();
    let mut args = args.clone();
    for name in ["confirm", "confirm_loss"] {
        if ctx.yes && command.arg(name).is_some() {
            args.insert(name.into(), Value::Bool(true));
        }
    }
    // Deleting and overwriting were confirmed before; what is left to ask
    // here is losing content on save.
    let engine_run =
        |args: &Map<String, Value>| run_within(&kernel, command, args, ctx.max_seconds);
    match engine_run(&args) {
        Err(e)
            if e.code == ErrorCode::ConfirmationRequired
                && command.arg("confirm_loss").is_some()
                && confirm(&format!("{}. Save anyway?", e.what), ctx) =>
        {
            args.insert("confirm_loss".into(), Value::Bool(true));
            engine_run(&args)
        }
        other => other,
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

/// Deleting or overwriting needs `--confirm`. `--yes`, or a person
/// answering yes at a terminal, gives it too; without a terminal or with
/// `--json` nobody is asked and the call is refused.
fn confirmed(
    command: &Command,
    args: &Map<String, Value>,
    ctx: &Context,
) -> Result<(), CommandError> {
    match check_confirmation(command, args) {
        Ok(()) => Ok(()),
        Err(_) if ctx.yes => Ok(()),
        Err(err) if confirm(&format!("{}. Go ahead?", err.what), ctx) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Fails early with a clear error when the input workbook is missing, even
/// for commands that are not built yet.
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
            v["implemented"] = Value::Bool(is_implemented(c));
            v
        })
        .collect();

    let mut text = String::new();
    for (heading, ready) in [("Ready:", true), ("Planned, not implemented yet:", false)] {
        text.push_str(heading);
        text.push('\n');
        for c in COMMANDS.iter().filter(|c| is_implemented(c) == ready) {
            let line = format!("  {:<13} {}", c.cli_name(), c.summary);
            text.push_str(&wrap(&line, 80, 16));
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str(
        "Run `carrysheet-cli <command> --help` for details, or add --json for schemas.\n",
    );

    let mut data = Map::new();
    data.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    data.insert("commands".into(), Value::Array(list));
    Done { data, text }
}

const ISSUES: &str = "https://github.com/yokoten-labs/carrysheet/issues/new";
const SUPPORT_EMAIL: &str = "support@carrysheet.com";

/// Takes the first line, cut to at most `max` characters.
fn title_from(message: &str, max: usize) -> String {
    let first = message
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if first.chars().count() <= max {
        first.to_owned()
    } else {
        let cut: String = first.chars().take(max.saturating_sub(3)).collect();
        format!("{}...", cut.trim_end())
    }
}

fn feedback(args: &Map<String, Value>) -> Result<Done, CommandError> {
    let kind = args["kind"].as_str().unwrap_or("bug");
    let message = redact_home(args["message"].as_str().unwrap_or("").trim());
    if message.is_empty() {
        return Err(CommandError::invalid_argument(
            "message",
            "the message is empty",
            "Say what happened in a sentence or two, like: carrysheet-cli feedback bug \"Saving as csv drops the second sheet\".",
        ));
    }
    let (heading, template) = match kind {
        "file" => ("What goes wrong with the file", "file.yml"),
        "idea" => ("What I am trying to do", "idea.yml"),
        _ => ("What happened", "bug.yml"),
    };
    let title = title_from(&message, 80);
    let body = format!(
        "## {heading}\n\n{message}\n\n## Environment\n\n- Carrysheet: carrysheet-cli {}\n- System: {} ({})\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let url = format!("{ISSUES}?template={template}");
    let attach = if kind == "file" {
        "\nIf you attach the file, run `carrysheet-cli sanitize` on it first and check the result.\n"
    } else {
        ""
    };
    let text = format!(
        "Title: {title}\n\n{body}\n---\nNothing was sent. To post this, open:\n  {url}\nor email it to {SUPPORT_EMAIL}.\n{attach}"
    );
    let mut data = Map::new();
    data.insert("kind".into(), json!(kind));
    data.insert("title".into(), json!(title));
    data.insert("body".into(), json!(body));
    data.insert("url".into(), json!(url));
    data.insert("email".into(), json!(SUPPORT_EMAIL));
    data.insert("sent".into(), json!(false));
    Ok(Done { data, text })
}

#[cfg(test)]
mod tests {
    use super::*;
    use carrysheet_commands::{command, validate_args};

    fn ctx() -> Context {
        Context {
            json: true,
            yes: false,
            interactive: false,
            max_seconds: None,
        }
    }

    fn run_json(name: &str, args: Value) -> Result<Done, CommandError> {
        let c = command(name).unwrap();
        let args = validate_args(c, &args)?;
        run(c, &args, &ctx())
    }

    #[test]
    fn implemented_names_exist() {
        for name in IMPLEMENTED {
            assert!(command(name).is_some(), "{name}");
        }
    }

    #[test]
    fn unbuilt_commands_say_so() {
        for c in COMMANDS.iter().filter(|c| !is_implemented(c)) {
            let mut args = Map::new();
            for a in c.all_args() {
                args.insert(a.name.into(), Value::Null);
            }
            if c.arg("path").is_some() {
                args.insert("path".into(), json!("Cargo.toml"));
            }
            let e = run(c, &args, &ctx()).unwrap_err();
            assert_eq!(e.code, ErrorCode::NotImplemented, "{}", c.name);
            assert!(e.what.contains(&c.cli_name()));
        }
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
        assert!(done.text.contains("Planned, not implemented yet:"));
        for line in done.text.lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
    }

    #[test]
    fn feedback_drafts_and_never_sends() {
        let done = run_json(
            "feedback",
            json!({ "kind": "file", "message": "Opening a.xls shows ### in column B\nmore detail" }),
        )
        .unwrap();
        assert_eq!(done.data["sent"], false);
        assert_eq!(done.data["title"], "Opening a.xls shows ### in column B");
        assert!(
            done.data["url"]
                .as_str()
                .unwrap()
                .ends_with("template=file.yml")
        );
        assert!(
            done.data["body"]
                .as_str()
                .unwrap()
                .contains(std::env::consts::OS)
        );
        assert!(done.text.contains("Nothing was sent"));
        assert!(done.text.contains("sanitize"));
    }

    #[test]
    fn feedback_hides_the_home_folder() {
        if let Some(home) = carrysheet_commands::paths::home_dir() {
            let msg = format!("Crash opening {}", home.join("a.xlsx").display());
            let done = run_json("feedback", json!({ "kind": "bug", "message": msg })).unwrap();
            let body = done.data["body"].as_str().unwrap();
            assert!(!body.contains(&home.display().to_string()), "{body}");
            assert!(body.contains("~"));
        }
    }

    #[test]
    fn feedback_refuses_an_empty_message() {
        let e = run_json("feedback", json!({ "kind": "idea", "message": "  " })).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn titles_are_cut_on_characters() {
        assert_eq!(title_from("\n\n  hello \nworld", 80), "hello");
        let long = "表".repeat(100);
        let t = title_from(&long, 80);
        assert_eq!(t.chars().count(), 80);
        assert!(t.ends_with("..."));
    }
}
