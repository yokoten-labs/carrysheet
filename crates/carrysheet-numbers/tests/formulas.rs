//! Formulas and merged ranges, checked against numbers-parser's tests.

use std::path::Path;

use carrysheet_numbers::{CellRange, Document, Formula, Table, Value};

fn open(name: &str) -> Document {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    Document::open(path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn formulas(table: &Table) -> Vec<Vec<Option<String>>> {
    table
        .rows()
        .map(|r| {
            r.cells()
                .map(|c| {
                    c.and_then(|c| c.formula())
                        .map(|f| f.text().unwrap().to_owned())
                })
                .collect()
        })
        .collect()
}

fn expect(rows: &[&[Option<&str>]]) -> Vec<Vec<Option<String>>> {
    rows.iter()
        .map(|r| r.iter().map(|c| c.map(str::to_owned)).collect())
        .collect()
}

const N: Option<&str> = None;

#[test]
fn formula_text() {
    // numbers-parser tests/test_formulas.py: TABLE_1_FORMULAS and TABLE_2_FORMULAS.
    let doc = open("formulas/test-10.numbers");
    assert_eq!(doc.warnings(), []);
    assert_eq!(
        formulas(&doc.sheets()[0].tables()[0]),
        expect(&[
            &[N, Some("A1"), Some("$B$1=1")],
            &[N, Some("A1+A2"), Some("A$2&B2")],
            &[N, Some("A1×A2"), Some("NOW()")],
            &[N, Some("A1-A2"), Some("NOW()+0.1")],
            &[N, Some("A1÷A2"), Some("$C4-C3")],
            &[N, Some("SUM(A1:A2)"), Some("IF(A6>6,TRUE,FALSE)")],
            &[N, Some("MEDIAN(A1:A2)"), Some("IF(A7>0,TRUE,FALSE)")],
            &[N, Some("AVERAGE(A1:A2)"), Some("A8≠10")],
            &[Some("A9"), N, N],
        ])
    );
    assert_eq!(
        formulas(&doc.sheets()[1].tables()[0]),
        expect(&[
            &[N, Some("A1&A2&A3")],
            &[N, Some("LEN(A2)+LEN(A3)")],
            &[N, Some("LEFT(A3,1)")],
            &[N, Some("MID(A4,2,2)")],
            &[N, Some("RIGHT(A5,2)")],
            &[N, Some("FIND(\"_\",A6)")],
            &[N, Some("FIND(\"YYY\",A7)")],
            &[N, Some("IF(FIND(\"_\",A8)>2,A1,A2)")],
            &[N, Some("100×(A9×2)%")],
            &[N, Some("IF(A10<5,\"smaller\",\"larger\")")],
            &[N, Some("IF(A11≤5,\"smaller\",\"larger\")")],
        ])
    );
    // Formula cells keep their last result.
    let t = &doc.sheets()[1].tables()[0];
    assert_eq!(t.value(0, 1), &Value::Text("XXX_1XXX_2XXX_3".into()));
    assert_eq!(t.value(1, 1), &Value::Number(10.0));
    assert_eq!(t.value(6, 1), &Value::Error, "FIND finds nothing");
    // A9 refers to itself: a circular reference ends in an error.
    assert_eq!(doc.sheets()[0].tables()[0].value(8, 0), &Value::Error);
}

#[test]
fn reference_errors() {
    // numbers-parser tests/test_issues.py: test_issue_42.
    let doc = open("formulas/issue-42.numbers");
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(t.value(6, 1), &Value::Error);
    let f = |r, c| t.cell(r, c).and_then(|c| c.formula()).cloned();
    assert_eq!(f(3, 1), Some(Formula::Text("#REF!×A4:A6".into())));
    assert_eq!(f(4, 1), Some(Formula::Text("#REF!×A5:A6".into())));
    // Whole-column references; Numbers shows them by header name ("sum").
    assert_eq!(f(6, 0), Some(Formula::Text("SUM(A)".into())));
}

#[test]
fn large_number_literals() {
    // numbers-parser tests/test_issues.py: test_issue_253 (Numbers 15.4).
    let doc = open("formulas/test-issue-253.numbers");
    let t = &doc.sheets()[0].tables()[0];
    let f = |r| {
        t.cell(r, 0)
            .and_then(|c| c.formula())
            .and_then(Formula::text)
            .map(str::to_owned)
    };
    assert_eq!(f(0).as_deref(), Some("12340000000000000"));
    assert_eq!(f(1).as_deref(), Some("12345670000000000"));
    assert_eq!(t.value(0, 1), &Value::Number(1.234e16));
}

fn ranges(table: &Table) -> Vec<String> {
    let mut r: Vec<String> = table
        .merged_ranges()
        .iter()
        .map(CellRange::to_string)
        .collect();
    r.sort();
    r
}

#[test]
fn merged_ranges() {
    // numbers-parser tests/test_merges.py.
    let doc = open("merges/test-9.numbers");
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(ranges(t), ["A2:B2", "B5:E5", "B6:B8", "C4:E4", "D7:E8"]);
    let t2 = &doc.sheets()[1].tables()[0];
    assert_eq!(ranges(t2), ["A1:B1", "B4:C5"]);

    // XXX_TABLE_1_REF: merged-away cells read as empty.
    let rows: Vec<Vec<Option<String>>> = t
        .rows()
        .map(|r| {
            r.values()
                .map(|v| match v {
                    Value::Text(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect()
        })
        .collect();
    let expected: &[&[Option<&str>]] = &[
        &[
            Some("XXX_COL_1"),
            Some("XXX_COL_2"),
            Some("XXX_COL_3"),
            Some("XXX_COL_4"),
            Some("XXX_COL_5"),
        ],
        &[
            Some("XXX_1_1__1_2"),
            N,
            Some("XXX_1_3"),
            Some("XXX_1_4"),
            Some("XXX_1_5"),
        ],
        &[
            Some("XXX_2_1"),
            Some("XXX_2_2"),
            Some("XXX_2_3"),
            Some("XXX_2_4"),
            Some("XXX_2_5"),
        ],
        &[Some("XXX_3_1"), Some("XXX_3_2"), Some("XXX_3_3__3_5"), N, N],
        &[Some("XXX_4_1"), Some("XXX_4_2__4_5"), N, N, N],
        &[
            Some("XXX_5_1"),
            Some("XXX_5_2__XXX_7_2"),
            Some("XXX_5_3"),
            Some("XXX_5_4"),
            Some("XXX_5_5"),
        ],
        &[
            Some("XXX_6_1"),
            N,
            Some("XXX_6_3"),
            Some("XXX_6_4__XXX_7_5"),
            N,
        ],
        &[Some("XXX_7_1"), N, Some("XXX_7_3"), N, N],
    ];
    assert_eq!(rows, expect(expected));

    // test_merge_references.
    let b2 = t.merge_at(1, 1).unwrap();
    assert_eq!(b2.to_string(), "A2:B2");
    assert_eq!(
        (b2.first_row, b2.first_column, b2.last_row, b2.last_column),
        (1, 0, 1, 1)
    );
    assert_eq!(t.merge_at(4, 2).unwrap().to_string(), "B5:E5");
    assert!(t.merge_at(0, 2).is_none());
    let d7 = t.merge_at(6, 3).unwrap();
    assert_eq!(
        (
            d7.last_row - d7.first_row + 1,
            d7.last_column - d7.first_column + 1
        ),
        (2, 2)
    );
    assert!(t2.merge_at(0, 0).is_some());
}

#[test]
fn merged_ranges_in_old_documents() {
    // numbers-parser tests/test_issues.py: test_issue_18 (merge region map).
    let doc = open("merges/issue-18.numbers");
    assert_eq!(ranges(&doc.sheets()[0].tables()[0]), ["B3:D3"]);
}
