use crate::*;
use std::ffi::OsString;

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn json_is_detected_before_double_dash_only() {
    assert!(wants_json(&os(&["x", "read", "--json"])));
    assert!(!wants_json(&os(&["x", "read", "--", "--json"])));
    assert!(!wants_json(&os(&["--json"])));
    assert!(wants_json(&os(&["x", "--json=true", "read"])));
    assert!(wants_json(&os(&["x", "read", "--json=false"])));
    assert!(!wants_json(&os(&["x", "read", "--jsonx"])));
}
