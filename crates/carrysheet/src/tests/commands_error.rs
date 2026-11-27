use crate::commands::error::*;

#[test]
fn exit_codes() {
    assert_eq!(ExitCode::Done.code(), 0);
    assert_eq!(ErrorCode::Io.exit_code().code(), 1);
    assert_eq!(ErrorCode::NotImplemented.exit_code(), ExitCode::Failed);
    assert_eq!(ErrorCode::Usage.exit_code().code(), 2);
    assert_eq!(ErrorCode::InvalidArgument.exit_code(), ExitCode::Usage);
    assert_eq!(ErrorCode::InvalidReference.exit_code(), ExitCode::Usage);
    assert_eq!(ErrorCode::NothingToDo.exit_code().code(), 3);
    assert_eq!(ErrorCode::Declined.exit_code(), ExitCode::Failed);
    assert_eq!(ErrorCode::ConfirmationRequired.exit_code().code(), 1);
    assert_eq!(
        serde_json::to_value(ErrorCode::ConfirmationRequired).unwrap(),
        ErrorCode::ConfirmationRequired.as_str()
    );
}

#[test]
fn json_shape() {
    let e = CommandError::not_implemented("mcp install");
    let v = e.to_envelope();
    assert_eq!(v["ok"], false);
    let err = v["error"].as_object().unwrap();
    let keys: Vec<_> = err.keys().map(String::as_str).collect();
    assert_eq!(keys, ["code", "what", "fix", "why"]);
    assert_eq!(err["code"], "not_implemented");
    assert!(err["what"].as_str().unwrap().contains("mcp install"));
    // The serde form matches the hand-written one.
    assert_eq!(serde_json::to_value(&e).unwrap(), e.to_json());
    assert_eq!(
        serde_json::to_string(&e).unwrap(),
        e.to_json().to_string(),
        "same key order"
    );
}

#[test]
fn codes_serialize_as_snake_case() {
    assert_eq!(
        serde_json::to_value(ErrorCode::NotImplemented).unwrap(),
        "not_implemented"
    );
    assert_eq!(ErrorCode::FileNotFound.to_string(), "file_not_found");
    assert_eq!(
        serde_json::to_value(ErrorCode::Internal).unwrap(),
        ErrorCode::Internal.as_str()
    );
    assert_eq!(ErrorCode::Internal.as_str(), "internal_error");
    assert_eq!(ErrorCode::DamagedFile.as_str(), "damaged_file");
    assert_eq!(ErrorCode::DamagedFile.to_string(), "damaged_file");
    assert_eq!(ErrorCode::LimitExceeded.to_string(), "limit_exceeded");
    assert_eq!(ErrorCode::LimitExceeded.exit_code(), ExitCode::Failed);
}

#[test]
fn text_has_three_parts() {
    let text = CommandError::new(ErrorCode::Io, "a", "b", "c").to_text();
    assert_eq!(text, "error: a\n  fix: c\n  why: b");
}

#[test]
fn every_way_of_showing_hides_the_home_folder() {
    let Some(home) = crate::commands::paths::home_dir() else {
        return;
    };
    let p = home.join("q1.xlsx").display().to_string();
    let e = CommandError::new(ErrorCode::Io, format!("cannot open {p}"), &p, &p);
    let home = home.display().to_string();
    for shown in [
        e.to_text(),
        e.to_json().to_string(),
        e.to_envelope().to_string(),
        serde_json::to_string(&e).unwrap(),
        e.to_string(),
    ] {
        assert!(!shown.contains(&home), "{shown}");
        assert!(shown.contains('~'), "{shown}");
    }
    // A reference error repeats what was typed.
    let typed = format!("'{p}'!A1");
    let e: CommandError = crate::model::SheetRange::parse(&typed).unwrap_err().into();
    assert!(e.what.contains(&home), "the field keeps the text");
    assert!(!e.to_json().to_string().contains(&home));
}

#[test]
fn ref_errors_become_usage_errors() {
    let e: CommandError = crate::model::CellRef::parse("XFE1").unwrap_err().into();
    assert_eq!(e.code, ErrorCode::InvalidReference);
    assert_eq!(e.exit_code(), ExitCode::Usage);
    assert!(e.fix.contains("A1:C10"));
}
