//! Runs the real `carrysheet-cli` binary and checks what agents rely on:
//! exit codes, JSON-only stdout, the error shape, refusing unknown flags,
//! and that `commands --json` passes the schema lint.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use carrysheet_commands::lint::{lint_schema, lint_tools};
use carrysheet_commands::schema::tool_definition;
use carrysheet_commands::table::{ArgType, COMMANDS, mcp_tools};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_carrysheet-cli");

/// A fresh, empty folder used as the home folder, so tests never read the
/// developer's real settings. Removed when dropped.
struct TempHome(PathBuf);

impl std::ops::Deref for TempHome {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_home() -> TempHome {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "carrysheet-cli-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempHome(dir)
}

fn run_in(home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("TZ", "UTC")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn run(args: &[&str]) -> Output {
    run_in(&temp_home(), args)
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exited normally")
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8(o.stderr.clone()).unwrap()
}

/// stdout must be exactly one JSON document on one line.
fn json_only(o: &Output) -> Value {
    let out = stdout(o);
    assert!(out.ends_with('\n'), "stdout: {out:?}");
    assert_eq!(out.trim_end().lines().count(), 1, "stdout: {out:?}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("not JSON ({e}): {out:?}"))
}

/// Checks the error envelope shape and returns the error object.
fn error_json(o: &Output) -> Value {
    let v = json_only(o);
    assert_eq!(v["ok"], false, "{v}");
    let top: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    assert_eq!(top, ["ok", "error"]);
    let err = v["error"].clone();
    let keys: Vec<_> = err.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["code", "what", "fix", "why"], "the fix comes early");
    for k in ["code", "what", "why", "fix"] {
        assert!(
            err[k].as_str().is_some_and(|s| !s.is_empty()),
            "{k} is empty in {err}"
        );
    }
    assert!(!stderr(o).is_empty(), "the message also goes to stderr");
    err
}

fn no_escape_codes(o: &Output) {
    assert!(!stdout(o).contains('\x1b'), "escape code on stdout");
    assert!(!stderr(o).contains('\x1b'), "escape code on stderr");
}

#[test]
fn no_arguments_is_a_usage_error() {
    let o = run(&[]);
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("Usage:"));
    no_escape_codes(&o);
}

#[test]
fn no_arguments_with_json_is_a_json_usage_error() {
    let o = run(&["--json"]);
    assert_eq!(code(&o), 2);
    assert_eq!(error_json(&o)["code"], "usage");
}

#[test]
fn version() {
    let o = run(&["--version"]);
    assert_eq!(code(&o), 0);
    assert_eq!(
        stdout(&o).trim(),
        format!("carrysheet-cli {}", env!("CARGO_PKG_VERSION"))
    );
    let o = run(&["--version", "--json"]);
    assert_eq!(code(&o), 0);
    let v = json_only(&o);
    assert_eq!(v["ok"], true);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn help_with_json_is_still_json() {
    let o = run(&["read", "--help", "--json"]);
    assert_eq!(code(&o), 0);
    let v = json_only(&o);
    assert!(v["help"].as_str().unwrap().contains("--max-rows"));
}

#[test]
fn help_fits_80_columns_without_colour() {
    for args in [
        vec!["--help"],
        vec!["read", "--help"],
        vec!["mcp", "--help"],
    ] {
        let o = run(&args);
        assert_eq!(code(&o), 0, "{args:?}");
        no_escape_codes(&o);
        for line in stdout(&o).lines() {
            assert!(line.chars().count() <= 80, "{args:?}: {line}");
        }
    }
}

#[test]
fn commands_json_is_json_only_and_every_schema_passes_the_lint() {
    let o = run(&["commands", "--json"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).is_empty());
    let v = json_only(&o);
    assert_eq!(v["ok"], true);
    let list = v["commands"].as_array().unwrap();
    assert_eq!(list.len(), COMMANDS.len());

    let mut tools = Vec::new();
    for (c, table) in list.iter().zip(COMMANDS) {
        assert_eq!(c["name"], table.name);
        let violations = lint_schema(&c["input_schema"]);
        assert!(violations.is_empty(), "{}: {violations:?}", table.name);
        for key in ["summary", "description", "cli"] {
            assert!(c[key].is_string(), "{} {key}", table.name);
        }
        assert!(c["implemented"].is_boolean());
        if c["mcp_tool"] == true {
            tools.push(serde_json::json!({
                "name": c["name"],
                "description": c["description"],
                "inputSchema": c["input_schema"],
                "annotations": c["annotations"],
            }));
        }
    }
    // The tool list built from the output is what the MCP server will send.
    let expected: Vec<Value> = mcp_tools().map(tool_definition).collect();
    assert_eq!(tools, expected);
    let violations = lint_tools(&tools);
    assert!(violations.is_empty(), "{violations:?}");
}

#[test]
fn commands_for_people_fit_80_columns() {
    let o = run(&["commands"]);
    assert_eq!(code(&o), 0);
    no_escape_codes(&o);
    let out = stdout(&o);
    assert!(out.contains("overview"));
    for line in out.lines() {
        assert!(line.chars().count() <= 80, "{line}");
    }
}

#[test]
fn unknown_flags_are_refused_with_a_suggestion() {
    let o = run(&["commands", "--jsno"]);
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    let err = stderr(&o);
    assert!(err.contains("--jsno") && err.contains("--json"), "{err}");
    no_escape_codes(&o);

    let o = run(&["--json", "read", "a.xlsx", "--max-row", "5"]);
    assert_eq!(code(&o), 2);
    let e = error_json(&o);
    assert_eq!(e["code"], "usage");
    assert!(e["what"].as_str().unwrap().contains("--max-row"));
    assert!(e["fix"].as_str().unwrap().contains("--max-rows"), "{e}");
    no_escape_codes(&o);
}

#[test]
fn usage_errors_name_what_is_missing_or_allowed() {
    let o = run(&["edit", "--json"]);
    assert_eq!(code(&o), 2);
    let e = error_json(&o);
    let what = e["what"].as_str().unwrap();
    assert!(what.contains("<PATH>") && what.contains("--ops"), "{e}");

    let o = run(&["read", "a.xlsx", "--mode", "everything", "--json"]);
    assert_eq!(code(&o), 2);
    let e = error_json(&o);
    assert!(
        e["fix"]
            .as_str()
            .unwrap()
            .contains("values, formulas, both"),
        "{e}"
    );
}

#[test]
fn json_with_a_value_is_refused_as_json() {
    for form in ["--json=true", "--json=false"] {
        let o = run(&["commands", form]);
        assert_eq!(code(&o), 2, "{form}");
        let e = error_json(&o);
        assert_eq!(e["code"], "usage");
        assert!(
            e["fix"]
                .as_str()
                .unwrap()
                .contains("--json without a value"),
            "{e}"
        );
        let o = run(&[form, "commands"]);
        assert_eq!(code(&o), 2, "{form}");
        assert_eq!(error_json(&o)["code"], "usage");
    }
}

#[test]
fn unknown_commands_are_refused_with_a_suggestion() {
    let o = run(&["overveiw", "--json"]);
    assert_eq!(code(&o), 2);
    let e = error_json(&o);
    assert!(e["fix"].as_str().unwrap().contains("overview"), "{e}");
}

#[test]
fn json_after_double_dash_is_not_a_flag() {
    let o = run(&["read", "--", "--json"]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    assert!(stdout(&o).is_empty(), "no JSON was asked for");
}

#[test]
fn dry_run_only_on_commands_that_change_files() {
    let home = temp_home();
    let book = home.join("book.xlsx");
    std::fs::write(&book, b"not really a workbook").unwrap();
    let book = book.to_str().unwrap();

    let o = run_in(&home, &["read", book, "--dry-run", "--json"]);
    assert_eq!(code(&o), 2);
    assert_eq!(error_json(&o)["code"], "usage");

    let o = run_in(&home, &["convert", book, "out.csv", "--dry-run", "--json"]);
    assert_eq!(code(&o), 1);
    assert_eq!(error_json(&o)["code"], "not_implemented");
}

#[test]
fn every_unbuilt_command_says_not_implemented() {
    let home = temp_home();
    let book = home.join("book.xlsx");
    std::fs::write(&book, b"x").unwrap();
    let book = book.to_str().unwrap().to_owned();

    let listing = json_only(&run(&["commands", "--json"]));
    for (c, table) in listing["commands"].as_array().unwrap().iter().zip(COMMANDS) {
        if c["implemented"] == true {
            continue;
        }
        let mut argv: Vec<String> = table.cli_path.iter().map(|s| s.to_string()).collect();
        for a in table.all_args().into_iter().filter(|a| a.required) {
            if !a.positional {
                argv.push(format!("--{}", a.name.replace('_', "-")));
            }
            argv.push(match a.ty {
                ArgType::Path if a.name == "path" => book.clone(),
                ArgType::Enum(values) => values[0].to_owned(),
                ArgType::ObjectList(_) => r#"[{"op":"clear","target":"A1"}]"#.to_owned(),
                _ => "x".to_owned(),
            });
        }
        argv.push("--json".to_owned());
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        let o = run_in(&home, &args);
        assert_eq!(code(&o), 1, "{args:?}: {}", stderr(&o));
        let e = error_json(&o);
        assert_eq!(e["code"], "not_implemented", "{args:?}");
        assert!(e["what"].as_str().unwrap().contains(&table.cli_name()));

        // Without --json, nothing goes to stdout.
        let plain: Vec<&str> = args[..args.len() - 1].to_vec();
        let o = run_in(&home, &plain);
        assert_eq!(code(&o), 1);
        assert!(stdout(&o).is_empty(), "{plain:?}");
        assert!(stderr(&o).contains("not implemented yet"));
    }
}

#[test]
fn missing_files_are_shown_with_tilde() {
    let home = temp_home();
    let missing = home.join("reports").join("q1.xlsx");
    let o = run_in(&home, &["overview", missing.to_str().unwrap(), "--json"]);
    assert_eq!(code(&o), 1);
    let e = error_json(&o);
    assert_eq!(e["code"], "file_not_found");
    let what = e["what"].as_str().unwrap();
    let sep = std::path::MAIN_SEPARATOR;
    assert!(what.starts_with(&format!("~{sep}reports")), "{what}");
    let home_text = home.display().to_string();
    assert!(!stdout(&o).contains(&home_text));
    assert!(!stderr(&o).contains(&home_text));
}

#[test]
fn typed_text_in_errors_is_shown_with_tilde() {
    let home = temp_home();
    let home_text = home.display().to_string();
    let book = home.join("b.xlsx");
    std::fs::write(&book, b"x").unwrap();
    let book = book.to_str().unwrap();
    let inside = home.join("x").display().to_string();
    let quoted = format!("'{inside}'!A1");
    let ops = r#"[{"op":"delete_sheet","target":"Old"}]"#;
    for args in [
        // clap repeats the bad value.
        vec!["read", book, "--mode", &inside, "--json"],
        vec!["read", book, "--mode", &inside],
        vec!["read", book, "--nope", &inside],
        // A reference error repeats the reference.
        vec!["read", book, &quoted, "--json"],
        vec!["read", book, &quoted],
        // The confirmation example repeats the call.
        vec!["edit", book, "--ops", ops, "--json"],
        vec!["edit", book, "--ops", ops],
    ] {
        let o = run_in(&home, &args);
        assert_ne!(code(&o), 0, "{args:?}");
        assert!(!stderr(&o).is_empty(), "{args:?}");
        assert!(!stdout(&o).contains(&home_text), "{args:?}: {}", stdout(&o));
        assert!(!stderr(&o).contains(&home_text), "{args:?}: {}", stderr(&o));
        no_escape_codes(&o);
    }
}

#[test]
fn bad_values_are_usage_errors() {
    let home = temp_home();
    let book = home.join("b.xlsx");
    std::fs::write(&book, b"x").unwrap();
    let book = book.to_str().unwrap();
    for args in [
        vec!["read", book, "--max-rows", "abc", "--json"],
        vec!["read", book, "--max-rows", "0", "--json"],
        vec!["read", book, "--mode", "everything", "--json"],
        vec!["read", book, "Sheet1!A1:XFE3", "--json"],
        vec!["read", book, "My sheet!A1", "--json"],
        vec!["edit", book, "--ops", "[{", "--json"],
        vec![
            "edit",
            book,
            "--ops",
            r#"[{"op":"explode","target":"A1"}]"#,
            "--json",
        ],
        vec!["eval", "--json"],
        vec!["mcp", "install", "notepad", "--json"],
    ] {
        let o = run_in(&home, &args);
        assert_eq!(code(&o), 2, "{args:?}: {}", stderr(&o));
        let e = error_json(&o);
        let c = e["code"].as_str().unwrap();
        assert!(
            ["usage", "invalid_argument", "invalid_reference"].contains(&c),
            "{args:?}: {e}"
        );
        no_escape_codes(&o);
    }
}

#[test]
fn feedback_drafts_without_sending() {
    let o = run(&[
        "feedback",
        "bug",
        "Saving as csv drops the second sheet",
        "--json",
    ]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let v = json_only(&o);
    assert_eq!(v["ok"], true);
    assert_eq!(v["sent"], false);
    assert_eq!(v["title"], "Saving as csv drops the second sheet");
    assert!(v["url"].as_str().unwrap().contains("template=bug.yml"));

    let o = run(&["feedback", "idea", "Sort by colour"]);
    assert_eq!(code(&o), 0);
    let out = stdout(&o);
    assert!(out.contains("Nothing was sent"));
    no_escape_codes(&o);
}

#[test]
fn feedback_hides_the_home_folder() {
    let home = temp_home();
    let msg = format!("Crash opening {}", home.join("a.xlsx").display());
    let o = run_in(&home, &["feedback", "bug", &msg, "--json"]);
    assert_eq!(code(&o), 0);
    let home_text = home.display().to_string();
    assert!(!stdout(&o).contains(&home_text), "{}", stdout(&o));
    assert!(json_only(&o)["body"].as_str().unwrap().contains('~'));
}

#[test]
fn yes_is_accepted_everywhere_and_nothing_waits_for_input() {
    // stdin is closed and there is no terminal, so nothing may prompt; the
    // command must finish on its own.
    let o = run(&["mcp", "install", "codex", "--json"]);
    assert_eq!(code(&o), 1);
    assert_eq!(error_json(&o)["code"], "not_implemented");
    let o = run(&["-y", "mcp", "install", "codex", "--json"]);
    assert_eq!(code(&o), 1);
}

#[test]
fn deleting_or_overwriting_needs_confirm() {
    let home = temp_home();
    let book = home.join("book.xlsx");
    std::fs::write(&book, b"x").unwrap();
    let book = book.to_str().unwrap();
    let ops = r#"[{"op":"set_value","target":"A1","value":3},{"op":"delete_rows","target":"Sheet1!2:3"}]"#;

    let o = run_in(&home, &["edit", book, "--ops", ops, "--json"]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    let e = error_json(&o);
    assert_eq!(e["code"], "confirmation_required");
    let fix = e["fix"].as_str().unwrap();
    assert!(fix.contains(r#""confirm":true"#), "{fix}");
    assert!(fix.contains("--confirm"), "{fix}");
    assert!(!fix.contains(&home.display().to_string()), "{fix}");

    // Without --json nothing is asked (no terminal) and nothing goes to stdout.
    let o = run_in(&home, &["edit", book, "--ops", ops]);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("confirm"), "{}", stderr(&o));

    for extra in [["--confirm"], ["--dry-run"], ["--yes"]] {
        let mut args = vec!["edit", book, "--ops", ops, "--json"];
        args.extend(extra);
        let o = run_in(&home, &args);
        assert_eq!(code(&o), 1, "{args:?}");
        assert_eq!(error_json(&o)["code"], "not_implemented", "{args:?}");
    }

    let o = run_in(
        &home,
        &["convert", book, "out.csv", "--overwrite", "--json"],
    );
    assert_eq!(code(&o), 1);
    assert_eq!(error_json(&o)["code"], "confirmation_required");
    let o = run_in(
        &home,
        &[
            "convert",
            book,
            "out.csv",
            "--overwrite",
            "--confirm",
            "--json",
        ],
    );
    assert_eq!(error_json(&o)["code"], "not_implemented");
}

#[test]
fn exit_code_3_is_reserved_for_nothing_to_do() {
    // No command returns it yet; the mapping lives in the command layer.
    assert_eq!(
        carrysheet_commands::ErrorCode::NothingToDo
            .exit_code()
            .code(),
        3
    );
}
