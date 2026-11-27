use crate::error::*;

#[test]
fn exit_codes() {
    assert_eq!(ExitCode::Done.code(), 0);
    assert_eq!(ErrorCode::Io.exit_code().code(), 1);
    assert_eq!(ErrorCode::Usage.exit_code().code(), 2);
    assert_eq!(ErrorCode::InvalidArgument.exit_code(), ExitCode::Usage);
    assert_eq!(ErrorCode::InvalidReference.exit_code(), ExitCode::Usage);
    assert_eq!(ErrorCode::NothingToDo.exit_code().code(), 3);
    assert_eq!(ErrorCode::ConfirmationRequired.exit_code().code(), 1);
    assert_eq!(
        serde_json::to_value(ErrorCode::ConfirmationRequired).unwrap(),
        ErrorCode::ConfirmationRequired.as_str()
    );
}

#[test]
fn json_shape() {
    let e = CommandError::invalid_argument("max_rows", "it is 0", "Use 1 or more.");
    let v = e.to_envelope();
    assert_eq!(v["ok"], false);
    let err = v["error"].as_object().unwrap();
    let keys: Vec<_> = err.keys().map(String::as_str).collect();
    assert_eq!(keys, ["code", "what", "fix", "why"]);
    assert_eq!(err["code"], "invalid_argument");
    assert!(err["what"].as_str().unwrap().contains("max_rows"));
    // The serde form matches the hand-written one.
    assert_eq!(serde_json::to_value(&e).unwrap(), e.to_json());
    assert_eq!(serde_json::to_string(&e).unwrap(), e.to_json().to_string(), "same key order");
}

#[test]
fn codes_serialize_as_snake_case() {
    assert_eq!(serde_json::to_value(ErrorCode::InvalidArgument).unwrap(), "invalid_argument");
    assert_eq!(ErrorCode::FileNotFound.to_string(), "file_not_found");
    assert_eq!(serde_json::to_value(ErrorCode::Internal).unwrap(), ErrorCode::Internal.as_str());
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
fn ref_errors_become_usage_errors() {
    let e: CommandError = crate::range::SheetRange::parse("XFE1").unwrap_err().into();
    assert_eq!(e.code, ErrorCode::InvalidReference);
    assert_eq!(e.exit_code(), ExitCode::Usage);
    assert!(e.fix.contains("A1:C10"));
}
