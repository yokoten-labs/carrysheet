//! Reads the fixtures and checks them against what numbers-parser's tests
//! say they contain (see tests/data/README.md for where each comes from).

use std::path::{Path, PathBuf};

use carrysheet_numbers::{Date, Document, NumbersError, Table, Value};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn open(name: &str) -> Document {
    let doc = Document::open(data(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(doc.warnings(), [], "{name}");
    doc
}

/// A value as test shorthand: `None` for empty, text as is, other values
/// through `Debug`.
fn short(v: &Value) -> Option<String> {
    match v {
        Value::Empty => None,
        Value::Text(t) => Some(t.to_string()),
        other => Some(format!("{other:?}")),
    }
}

fn grid(table: &Table) -> Vec<Vec<Option<String>>> {
    table
        .rows()
        .map(|r| r.values().map(short).collect())
        .collect()
}

fn texts(rows: &[&[Option<&str>]]) -> Vec<Vec<Option<String>>> {
    rows.iter()
        .map(|r| r.iter().map(|c| c.map(str::to_owned)).collect())
        .collect()
}

const N: Option<&str> = None;

#[test]
fn sheets_tables_and_text() {
    // numbers-parser tests/test_tables.py: test_names_refs, test_table_contents,
    // test_empty_cells.
    let doc = open("cells/test-1.numbers");
    let names: Vec<_> = doc.sheets().iter().map(|s| s.name()).collect();
    assert_eq!(names, ["ZZZ_Sheet_1", "ZZZ_Sheet_2"]);
    let sheet = doc.sheet("ZZZ_Sheet_1").unwrap();
    let tables: Vec<_> = sheet.tables().iter().map(Table::name).collect();
    assert_eq!(tables, ["ZZZ_Table_1", "ZZZ_Table_2"]);
    let t1 = sheet.table("ZZZ_Table_1").unwrap();
    assert_eq!((t1.row_count(), t1.column_count()), (5, 3));
    assert_eq!((t1.header_rows(), t1.header_columns()), (1, 1));
    assert_eq!(
        grid(t1),
        texts(&[
            &[N, Some("YYY_COL_1"), Some("YYY_COL_2")],
            &[Some("YYY_ROW_1"), Some("YYY_1_1"), Some("YYY_1_2")],
            &[Some("YYY_ROW_2"), Some("YYY_2_1"), Some("YYY_2_2")],
            &[Some("YYY_ROW_3"), Some("YYY_3_1"), Some("YYY_3_2")],
            &[Some("YYY_ROW_4"), Some("YYY_4_1"), Some("YYY_4_2")],
        ])
    );
    assert_eq!(
        grid(&sheet.tables()[1]),
        texts(&[
            &[N, Some("ZZZ_COL_1"), Some("ZZZ_COL_2"), Some("ZZZ_COL_3")],
            &[
                Some("ZZZ_ROW_1"),
                Some("ZZZ_1_1"),
                Some("ZZZ_1_2"),
                Some("ZZZ_1_3")
            ],
            &[
                Some("ZZZ_ROW_2"),
                Some("ZZZ_2_1"),
                Some("ZZZ_2_2"),
                Some("ZZZ_2_3")
            ],
            &[
                Some("ZZZ_ROW_3"),
                Some("ZZZ_3_1"),
                Some("ZZZ_3_2"),
                Some("ZZZ_3_3")
            ],
        ])
    );
    assert_eq!(t1.value(1, 0), &Value::Text("YYY_ROW_1".into()));
    assert!(t1.cell(0, 0).is_none());
    assert!(t1.cell(99, 0).is_none(), "outside the table");

    let t = doc
        .sheet("ZZZ_Sheet_2")
        .unwrap()
        .table("XXX_Table_1")
        .unwrap();
    assert_eq!((t.row_count(), t.column_count()), (4, 6));
    assert_eq!(grid(t), xxx_table_1());
}

/// numbers-parser tests/test_folder.py and test_tables.py: XXX_TABLE_1_REF.
fn xxx_table_1() -> Vec<Vec<Option<String>>> {
    texts(&[
        &[
            N,
            Some("XXX_COL_1"),
            Some("XXX_COL_2"),
            Some("XXX_COL_3"),
            Some("XXX_COL_4"),
            Some("XXX_COL_5"),
        ],
        &[
            Some("XXX_ROW_1"),
            Some("XXX_1_1"),
            Some("XXX_1_2"),
            Some("XXX_1_3"),
            Some("XXX_1_4"),
            Some("XXX_1_5"),
        ],
        &[
            Some("XXX_ROW_2"),
            Some("XXX_2_1"),
            Some("XXX_2_2"),
            N,
            Some("XXX_2_4"),
            Some("XXX_2_5"),
        ],
        &[
            Some("XXX_ROW_3"),
            Some("XXX_3_1"),
            N,
            Some("XXX_3_3"),
            Some("XXX_3_4"),
            Some("XXX_3_5"),
        ],
    ])
}

#[test]
fn value_types() {
    // numbers-parser tests/test_properties.py: test_cell_types (ZZZ_TABLE_2_REF).
    let doc = open("cells/test-4.numbers");
    let t = doc.sheets()[0].table("ZZZ_Table_2").unwrap();
    let days = |d: f64| Value::Date(Date::from_seconds_since_2001(d * 86_400.0));
    let rows: Vec<Vec<Value>> = t.rows().map(|r| r.values().cloned().collect()).collect();
    assert_eq!(
        rows[2],
        [
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(123.456)
        ]
    );
    assert_eq!(rows[3], [days(0.0), days(19.0), days(365.0)]);
    assert_eq!(
        rows[3][1],
        Value::Date(Date::from_seconds_since_2001(1_641_600.0))
    );
    if let Value::Date(d) = &rows[3][2] {
        assert_eq!(d.to_string(), "2002-01-01T00:00:00");
        assert_eq!(d.excel_serial(), Some(37_257.0));
    }
    assert_eq!(
        rows[4],
        [
            Value::Duration(1.001),
            Value::Duration(86_400.0),
            Value::Duration(864_000.0)
        ]
    );
    assert_eq!(
        rows[5],
        [Value::Bool(false), Value::Bool(true), Value::Bool(false)]
    );
    assert_eq!(rows[6][0], Value::Text("STRING1".into()));

    // test_properties.py: ZZZ_TABLE_1_REF, with A2:B2 merged.
    let t = doc.sheets()[0].table("ZZZ_Table_1").unwrap();
    assert_eq!(
        grid(t),
        texts(&[
            &[Some("YYY_1_1"), Some("YYY_1_2")],
            &[Some("YYY_2a_1"), N],
            &[Some("YYY_2b_1"), N],
            &[Some("YYY_3_1"), Some("YYY_3_2")],
            &[Some("YYY_4_1"), Some("YYY_4_2")],
        ])
    );
}

#[test]
fn empty_rows() {
    // numbers-parser tests/test_tables.py: EMPTY_ROWS_REF.
    let doc = open("cells/test-empty-rows.numbers");
    let t = &doc.sheets()[0].tables()[0];
    let r = |a: &'static str, b: Option<&'static str>, c: &'static str, f: Option<&'static str>| {
        vec![Some(a), b, Some(c), N, N, f]
    };
    let empty = vec![N; 6];
    let expected = [
        r("A", Some("B"), "C", N),
        r("A_1", Some("B_1"), "C_1", N),
        r("A_2", Some("B_2"), "C_2", N),
        empty.clone(),
        empty.clone(),
        r("A_5", Some("B_5"), "C_5", N),
        r("A_6", Some("B_6"), "C_6", N),
        empty,
        r("A_8", N, "C_8", Some("F_8")),
        r("A_9", N, "C_9", N),
    ];
    let expected: Vec<&[Option<&str>]> = expected.iter().map(Vec::as_slice).collect();
    assert_eq!(grid(t), texts(&expected));
    assert_eq!(t.non_empty_rows().count(), 7);
}

#[test]
fn versions() {
    let doc = open("cells/test-1.numbers");
    assert_eq!(doc.numbers_version(), Some("11.0.3"));
    assert_eq!(
        doc.build_history(),
        [
            "Template: Blank (10.2)",
            "M10.3.9-7029.9.8-4",
            "M11.0-7030.0.94-2"
        ]
    );
    assert_eq!(
        open("formulas/test-10.numbers").numbers_version(),
        Some("14.2.2")
    );
    assert_eq!(
        open("formulas/test-issue-253.numbers").numbers_version(),
        Some("26.4.0")
    );
    // issue-18 has empty property lists (numbers-parser test_issue_18):
    // it reads, with one warning that the version is unknown.
    let doc = Document::open(data("merges/issue-18.numbers")).unwrap();
    assert_eq!(doc.numbers_version(), None);
    assert!(doc.build_history().is_empty());
    assert_eq!(doc.warnings().len(), 1);
    assert!(
        doc.warnings()[0]
            .message
            .contains("does not say which Numbers version"),
        "{}",
        doc.warnings()[0]
    );
}

#[test]
fn from_bytes_matches_open() {
    let bytes = std::fs::read(data("cells/test-1.numbers")).unwrap();
    let doc = Document::from_bytes(&bytes).unwrap();
    assert_eq!(doc.sheets().len(), 2);
    assert_eq!(grid(&doc.sheets()[1].tables()[0]), xxx_table_1());
}

/// Unpacks a zipped package folder into a fresh temporary folder and
/// returns the path of the `.numbers` folder in it.
fn unpack(zip_name: &str, folder: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "carrysheet-numbers-{}-{}",
        std::process::id(),
        zip_name.replace('/', "-")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let file = std::fs::File::open(data(zip_name)).unwrap();
    zip::ZipArchive::new(file).unwrap().extract(&dir).unwrap();
    dir.join(folder)
}

#[test]
fn package_folder_with_index_folder() {
    // numbers-parser tests/test_folder.py.
    let path = unpack("packages/test-5.zip", "test-5.numbers");
    let doc = Document::open(&path).unwrap();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert_eq!(doc.numbers_version(), Some("11.0.3"));
    let t = doc
        .sheet("ZZZ_Sheet_1")
        .unwrap()
        .table("XXX_Table_1")
        .unwrap();
    assert_eq!(grid(t), xxx_table_1());
}

#[test]
fn package_folder_with_index_zip() {
    // numbers-parser tests/test_slices.py.
    let path = unpack("packages/test-7.zip", "test-7.numbers");
    let doc = Document::open(&path).unwrap();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert_eq!(doc.warnings(), []);
    let sheet = doc.sheet("ZZZ_Sheet_1").unwrap();
    let t = sheet.table("XXX_Table_1").unwrap();
    assert_eq!((t.column_count(), t.row_count()), (5, 4));
    assert_eq!(t.value(0, 1), &Value::Text("XXX_COL_2".into()));
    assert_eq!(t.value(2, 2), &Value::Empty);
    assert_eq!(t.value(3, 4), &Value::Text("XXX_3_5".into()));

    let num = |v: &Value| match v {
        Value::Number(n) => *n,
        _ => 0.0,
    };
    let t = sheet.table("XXX_Table_2").unwrap();
    let first_column: f64 = t.rows().map(|r| num(r.values().next().unwrap())).sum();
    assert_eq!(first_column, 252.0);
    let all: f64 = t
        .rows()
        .flat_map(|r| r.values().map(num).collect::<Vec<_>>())
        .sum();
    assert!((all - 1792.802).abs() < 1e-9, "{all}");

    let t = doc
        .sheet("ZZZ_Sheet_2")
        .unwrap()
        .table("XXX_Table_1")
        .unwrap();
    assert_eq!(t.value(0, 0), &Value::Text("excepteur".into()));
    assert_eq!(t.value(0, 25), &Value::Text("ea".into()));
    assert_eq!(t.value(0, 52), &Value::Text("veniam".into()));
    assert_eq!(t.value(0, 103), &Value::Text("dolore".into()));
}

#[test]
fn unsupported_documents_say_why() {
    // numbers-parser tests/test_coverage.py: "Pre-BNC storage is unsupported".
    let e = Document::open(data("unsupported/pre-bnc.numbers")).unwrap_err();
    assert!(matches!(e, NumbersError::Unsupported { .. }), "{e}");
    let e = Document::open(data("unsupported/encrypted.numbers")).unwrap_err();
    assert!(matches!(e, NumbersError::PasswordProtected), "{e}");
    assert!(e.fix().contains("password"));
    let e = Document::open(data("does-not-exist.numbers")).unwrap_err();
    assert!(matches!(e, NumbersError::Io(_)), "{e}");
    let e = Document::from_bytes(b"not a zip").unwrap_err();
    assert!(matches!(e, NumbersError::NotNumbers { .. }), "{e}");
    let e = Document::open(data("cells")).unwrap_err();
    assert!(matches!(e, NumbersError::NotNumbers { .. }), "{e}");
}
