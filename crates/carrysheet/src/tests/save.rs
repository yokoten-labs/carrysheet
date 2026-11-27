use crate::error::ErrorCode;
use crate::save::*;
use std::path::Path;

#[test]
fn file_text_is_quoted_escaped_and_cut() {
    assert_eq!(part_name("xl/a.xml"), "\"xl/a.xml\"");
    assert_eq!(part_name("a\u{202E}b\n\""), "\"a\\u{202e}b\\u{a}\\\"\"");
    let cut = format!("\"{}\"…[cut: 36 more characters]", "x".repeat(64));
    assert_eq!(part_name(&"x".repeat(100)), cut);
}

#[test]
fn reports_say_what_is_lost() {
    let mut r = SaveReport::new(FileFormat::Xlsx);
    assert!(!r.loses_something());
    r.lost.push(Loss {
        feature: "charts".into(),
        before: 3,
        after: 1,
    });
    assert!(r.loses_something());
    assert_eq!(r.summary(), "charts (2 of 3)");
}

#[test]
fn outputs_never_replace_a_file_unless_asked() {
    let taken = |p: &Path| p.ends_with("book.xlsx") || p.ends_with("book-edited.xlsx");
    let book = Path::new("dir/book.xlsx");
    let free = |_: &Path| false;
    assert_eq!(
        default_output(book, &free).unwrap(),
        Path::new("dir/book-edited.xlsx")
    );
    assert_eq!(
        default_output(book, &taken).unwrap(),
        Path::new("dir/book-edited-2.xlsx")
    );
    assert_eq!(
        check_output(book, false, &taken).unwrap_err().code,
        ErrorCode::FileExists
    );
    assert!(check_output(book, true, &taken).is_ok());
}

#[test]
fn file_formats_from_paths() {
    assert_eq!(
        FileFormat::from_path(Path::new("a.XLSX")),
        Some(FileFormat::Xlsx)
    );
    assert_eq!(
        FileFormat::from_path(Path::new("a.xls")),
        Some(FileFormat::Xls)
    );
    assert_eq!(
        FileFormat::from_path(Path::new("a.csv")),
        Some(FileFormat::Csv)
    );
    assert_eq!(FileFormat::from_path(Path::new("a.ods")), None);
    assert_eq!(FileFormat::from_path(Path::new("a")), None);
}
