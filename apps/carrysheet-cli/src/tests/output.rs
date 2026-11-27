use crate::cli::BIN;
use crate::output::*;
use carrysheet::commands::{CommandError, ErrorCode};
use serde_json::{Map, Value};

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
    let e = usage_of(&[BIN, "eval", "a.xlsx"]);
    assert!(e.what.contains("<FORMULA>"), "{}", e.what);
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
    let e = usage_of(&[BIN, "convert", "a.xlsx", "b.x", "--format", "ods"]);
    assert!(e.fix.contains("xlsx, xls, csv"), "{}", e.fix);
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
