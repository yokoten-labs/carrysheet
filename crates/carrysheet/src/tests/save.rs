use crate::error::ErrorCode;
use crate::save::*;
use std::path::Path;

#[test]
fn reports_say_what_is_lost() {
    let mut r = SaveReport::new(FileFormat::Xlsx);
    assert!(!r.loses_something());
    r.lost.push(Loss { feature: "charts".into(), before: 3, after: 1 });
    assert!(r.loses_something());
    assert_eq!(r.summary(), "charts (2 of 3)");
}

#[test]
fn outputs_never_replace_a_file_unless_asked() {
    let taken = |p: &Path| p.ends_with("book.xlsx") || p.ends_with("book-edited.xlsx");
    let book = Path::new("dir/book.xlsx");
    let free = |_: &Path| false;
    assert_eq!(default_output(book, &free).unwrap(), Path::new("dir/book-edited.xlsx"));
    assert_eq!(default_output(book, &taken).unwrap(), Path::new("dir/book-edited-2.xlsx"));
    assert_eq!(check_output(book, false, &taken).unwrap_err().code, ErrorCode::FileExists);
    assert!(check_output(book, true, &taken).is_ok());
}

#[test]
fn file_formats_from_extensions() {
    assert_eq!(FileFormat::from_extension("XLSX"), Some(FileFormat::Xlsx));
    assert_eq!(FileFormat::from_extension("xlsm"), Some(FileFormat::Xlsx));
    assert_eq!(FileFormat::from_extension("csv"), Some(FileFormat::Csv));
    assert_eq!(FileFormat::from_extension("xls"), None);
    assert_eq!(FileFormat::from_extension("ods"), None);
    let e = output_format(Path::new("a.xls"), None).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedFormat);
    let by_flag = output_format(Path::new("a"), Some("csv")).unwrap();
    assert_eq!(by_flag, FileFormat::Csv);
}
