use crate::commands::engine::*;
use crate::model::CellValue;
use serde_json::{Value, json};
use std::path::Path;

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

#[test]
fn non_finite_numbers_become_text() {
    assert_eq!(value_json(&CellValue::Number(f64::NAN)), json!("NaN"));
    assert_eq!(value_json(&CellValue::Empty), Value::Null);
}
