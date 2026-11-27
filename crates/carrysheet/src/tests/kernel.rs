use crate::commands::{CommandError, ErrorCode};
use crate::kernel::*;

#[test]
fn panics_become_internal_errors_not_success() {
    let r: Result<(), CommandError> = guard("testing", || panic!("boom at A1"));
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::Internal);
    assert_eq!(e.code.as_str(), "internal_error");
    assert!(e.why.contains("boom at A1"), "{}", e.why);
    assert!(e.fix.contains("/issues"), "{}", e.fix);
    let r: Result<u8, CommandError> = guard("testing", || panic!("{}", String::from("x")));
    assert!(r.unwrap_err().why.contains('x'));
}
