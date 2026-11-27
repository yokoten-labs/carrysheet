use crate::run::*;
use carrysheet::commands::table::COMMANDS;
use carrysheet::commands::{CommandError, ErrorCode};
use carrysheet::commands::{command, validate_args};
use serde_json::{Value, json};

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
    let plain = done.plain.unwrap();
    assert_eq!(plain["commands"].as_array().unwrap().len(), COMMANDS.len());
    assert!(
        !plain["commands"][0]
            .as_object()
            .unwrap()
            .contains_key("input_schema")
    );
}

// Issue 102: `commands` says every listed command is implemented, so
// none may fall through to `not_implemented`.
#[test]
fn every_listed_command_runs() {
    use carrysheet::ENGINE_COMMANDS;
    for c in COMMANDS {
        let built = matches!(c.name, "commands" | "mcp_serve");
        assert!(built || ENGINE_COMMANDS.contains(&c.name), "{}", c.name);
    }
}
