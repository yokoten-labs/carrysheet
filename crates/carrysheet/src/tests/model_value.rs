use crate::model::value::*;

#[test]
fn error_kinds_round_trip() {
    for k in ErrorKind::ALL {
        assert_eq!(ErrorKind::parse(k.as_str()), Some(k));
        assert_eq!(ErrorKind::parse(&k.as_str().to_lowercase()), Some(k));
    }
    assert_eq!(ErrorKind::parse("#WHAT"), None);
    assert_eq!(ErrorKind::Div0.to_string(), "#DIV/0!");
}

#[test]
fn values_display_and_kind() {
    assert_eq!(CellValue::from(3.5).to_string(), "3.5");
    assert_eq!(CellValue::from(true).to_string(), "TRUE");
    assert_eq!(CellValue::from("hi").to_string(), "hi");
    assert_eq!(CellValue::from(ErrorKind::NA).to_string(), "#N/A");
    assert_eq!(CellValue::Empty.to_string(), "");
    assert_eq!(CellValue::Empty.kind(), "empty");
    assert_eq!(CellValue::from(String::from("x")).kind(), "text");
    assert!(CellValue::default().is_empty());
}
