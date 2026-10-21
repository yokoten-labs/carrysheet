//! Queries over small in-code tables.
#![cfg(not(target_family = "wasm"))]

use carrysheet_model::{CellValue, ErrorKind};
use carrysheet_query::{
    Affinity, Header, Limits, Page, QueryCell, QueryEngine, QueryErrorCode, TableInput,
};
use serde_json::{Value, json};

fn e() -> QueryCell {
    QueryCell::default()
}

fn sales() -> TableInput {
    TableInput::new(
        "Sales 2024",
        vec![
            vec![
                "Region".into(),
                "Product".into(),
                "Units".into(),
                "Price".into(),
            ],
            vec!["East".into(), "Pen".into(), 10.0.into(), 1.5.into()],
            vec!["West".into(), "Pen".into(), 4.0.into(), 1.5.into()],
            vec!["East".into(), "Ink".into(), 3.0.into(), 7.25.into()],
            vec!["North".into(), "Pad".into(), e(), 2.0.into()],
        ],
    )
}

fn regions() -> TableInput {
    TableInput::new(
        "地区",
        vec![
            vec!["区域".into(), "经理".into()],
            vec!["East".into(), "王芳".into()],
            vec!["West".into(), "李雷".into()],
        ],
    )
}

fn engine(tables: Vec<TableInput>) -> QueryEngine {
    QueryEngine::load(&tables, Limits::default()).expect("load")
}

fn rows(engine: &QueryEngine, sql: &str) -> Vec<Vec<Value>> {
    engine
        .query(sql, Page::default())
        .unwrap_or_else(|e| panic!("{sql}\n{e}"))
        .rows
}

#[test]
fn group_by_on_a_sheet_with_spaces_in_its_name() {
    let q = engine(vec![sales()]);
    let r = rows(
        &q,
        "SELECT Region, SUM(Units) AS units FROM \"Sales 2024\" GROUP BY Region ORDER BY Region",
    );
    assert_eq!(
        r,
        vec![
            vec![json!("East"), json!(13)],
            vec![json!("North"), json!(null)],
            vec![json!("West"), json!(4)],
        ]
    );
}

#[test]
fn join_of_two_sheets_with_cjk_names() {
    let q = engine(vec![sales(), regions()]);
    let r = rows(
        &q,
        "SELECT s.Region, r.\"经理\", SUM(s.Units * s.Price) AS revenue \
         FROM \"Sales 2024\" s JOIN \"地区\" r ON r.\"区域\" = s.Region \
         GROUP BY s.Region ORDER BY revenue DESC",
    );
    assert_eq!(
        r,
        vec![
            vec![json!("East"), json!("王芳"), json!(36.75)],
            vec![json!("West"), json!("李雷"), json!(6.0)],
        ]
    );
}

#[test]
fn columns_report_names_and_declared_types() {
    let q = engine(vec![sales()]);
    let result = q
        .query(
            "SELECT Region, Units, Price, Units + 1 FROM \"Sales 2024\"",
            Page::default(),
        )
        .unwrap();
    let cols: Vec<(&str, Option<&str>)> = result
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.declared_type.as_deref()))
        .collect();
    assert_eq!(
        cols,
        [
            ("Region", Some("TEXT")),
            ("Units", Some("INTEGER")),
            ("Price", Some("REAL")),
            ("Units + 1", None),
        ]
    );
    assert_eq!(result.row_count, 4);
    assert!(!result.truncated);
    assert_eq!(result.next_offset, None);
    let info = &q.tables()[0];
    assert_eq!(info.name, "Sales 2024");
    assert_eq!(info.rows, 4);
    assert_eq!(info.columns[2].affinity, Affinity::Integer);
    assert_eq!(info.columns[3].affinity, Affinity::Real);
}

#[test]
fn empty_and_duplicate_headers() {
    let t = TableInput::new(
        "Sheet1",
        vec![
            vec!["Name".into(), e(), "Name".into(), "name".into()],
            vec!["a".into(), 1.0.into(), "b".into(), "c".into()],
        ],
    );
    let q = engine(vec![t]);
    let names: Vec<&str> = q.tables()[0]
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["Name", "B", "Name_2", "name_3"]);
    assert_eq!(
        rows(&q, "SELECT Name, B, Name_2, name_3 FROM Sheet1"),
        vec![vec![json!("a"), json!(1), json!("b"), json!("c")]]
    );
}

#[test]
fn headers_given_or_as_letters() {
    let data = vec![vec![1.0.into(), 2.0.into()], vec![3.0.into(), 4.0.into()]];
    let letters = TableInput::new("L", data.clone()).with_header(Header::Letters);
    let named = TableInput::new("N", data).with_header(Header::Names(vec!["x".into()]));
    let q = engine(vec![letters, named]);
    assert_eq!(
        rows(&q, "SELECT SUM(A), SUM(B) FROM L"),
        vec![vec![json!(4), json!(6)]]
    );
    assert_eq!(
        rows(&q, "SELECT SUM(x), SUM(B) FROM N"),
        vec![vec![json!(4), json!(6)]]
    );
}

#[test]
fn table_names_with_quotes_and_repeats() {
    let a = TableInput::new("Bob's \"data\"", vec![vec!["v".into()], vec![1.0.into()]]);
    let b = TableInput::new("bob's \"DATA\"", vec![vec!["v".into()], vec![2.0.into()]]);
    let q = engine(vec![a, b]);
    assert_eq!(q.tables()[1].name, "bob's \"DATA\"_2");
    assert_eq!(
        rows(
            &q,
            "SELECT v FROM \"Bob's \"\"data\"\"\" UNION ALL SELECT v FROM \"bob's \"\"DATA\"\"_2\""
        ),
        vec![vec![json!(1)], vec![json!(2)]]
    );
}

#[test]
fn nulls_mixed_types_booleans_errors_and_dates() {
    let t = TableInput::new(
        "T",
        vec![
            vec!["mixed".into(), "flag".into(), "when".into(), "zip".into()],
            vec![
                1.0.into(),
                true.into(),
                QueryCell::Date("2024-03-01".into()),
                "02134".into(),
            ],
            vec![
                "12".into(),
                false.into(),
                QueryCell::Date("2024-01-15".into()),
                "10001".into(),
            ],
            vec![CellValue::Error(ErrorKind::NA).into(), e(), e(), e()],
            vec![2.5.into()],
        ],
    );
    let q = engine(vec![t]);
    let info = &q.tables()[0];
    let types: Vec<Affinity> = info.columns.iter().map(|c| c.affinity).collect();
    assert_eq!(
        types,
        [
            Affinity::Mixed,
            Affinity::Boolean,
            Affinity::Date,
            Affinity::Text
        ]
    );
    let result = q
        .query(
            "SELECT mixed, typeof(mixed), flag, \"when\", zip FROM T",
            Page::default(),
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![
                json!(1),
                json!("integer"),
                json!(true),
                json!("2024-03-01"),
                json!("02134")
            ],
            // Text that looks like a number stays text in a mixed column.
            vec![
                json!("12"),
                json!("text"),
                json!(false),
                json!("2024-01-15"),
                json!("10001")
            ],
            vec![
                json!("#N/A"),
                json!("text"),
                json!(null),
                json!(null),
                json!(null)
            ],
            vec![
                json!(2.5),
                json!("real"),
                json!(null),
                json!(null),
                json!(null)
            ],
        ]
    );
    assert!(result.notes.iter().any(|n| n.contains("ISO 8601")));
    assert_eq!(
        rows(&q, "SELECT COUNT(*) FROM T WHERE flag"),
        vec![vec![json!(1)]]
    );
    assert_eq!(
        rows(&q, "SELECT \"when\" FROM T WHERE \"when\" < '2024-02-01'"),
        vec![vec![json!("2024-01-15")]]
    );
    assert_eq!(
        rows(&q, "SELECT COUNT(*), COUNT(flag) FROM T"),
        vec![vec![json!(4), json!(2)]]
    );
}

#[test]
fn empty_sheet_and_no_tables() {
    let q = engine(vec![TableInput::new("Empty", vec![])]);
    assert_eq!(q.tables()[0].columns[0].name, "A");
    assert_eq!(rows(&q, "SELECT COUNT(*) FROM Empty"), vec![vec![json!(0)]]);
    let none = engine(vec![]);
    assert_eq!(rows(&none, "SELECT 1 + 1"), vec![vec![json!(2)]]);
}

#[test]
fn table_info_pragma_is_allowed() {
    let q = engine(vec![sales()]);
    let r = rows(&q, "PRAGMA table_info(\"Sales 2024\")");
    assert_eq!(r.len(), 4);
    assert_eq!(r[0][1], json!("Region"));
    assert_eq!(r[0][2], json!("TEXT"));
}

#[test]
fn paging_walks_the_whole_result() {
    let mut data = vec![vec!["n".into()]];
    data.extend((1..=25).map(|i| vec![QueryCell::from(f64::from(i))]));
    let q = engine(vec![TableInput::new("Nums", data)]);
    let sql = "SELECT n FROM Nums ORDER BY n";
    let mut seen = Vec::new();
    let mut page = Page {
        offset: 0,
        max_rows: 10,
    };
    let mut pages = 0;
    loop {
        let r = q.query(sql, page).unwrap();
        pages += 1;
        assert_eq!(r.offset, page.offset);
        assert_eq!(r.row_count, r.rows.len());
        seen.extend(r.rows.iter().map(|row| row[0].as_i64().unwrap()));
        match r.next_offset {
            Some(next) => {
                assert!(r.truncated);
                assert!(
                    r.notes
                        .iter()
                        .any(|n| n.contains(&format!("offset {next}")))
                );
                page.offset = next;
            }
            None => {
                assert!(!r.truncated);
                break;
            }
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, (1..=25).collect::<Vec<i64>>());

    // A page exactly at the end is not truncated; past the end is empty.
    let r = q
        .query(
            sql,
            Page {
                offset: 15,
                max_rows: 10,
            },
        )
        .unwrap();
    assert_eq!((r.row_count, r.truncated), (10, false));
    let r = q
        .query(
            sql,
            Page {
                offset: 100,
                max_rows: 10,
            },
        )
        .unwrap();
    assert_eq!((r.row_count, r.truncated, r.next_offset), (0, false, None));
}

#[test]
fn page_size_is_capped_by_limits() {
    let mut data = vec![vec!["n".into(), "m".into()]];
    data.extend((1..=50).map(|i| vec![QueryCell::from(f64::from(i)), e()]));
    let limits = Limits {
        max_page_rows: 30,
        max_page_cells: 40,
        ..Limits::default()
    };
    let q = QueryEngine::load(&[TableInput::new("T", data)], limits).unwrap();
    let r = q
        .query(
            "SELECT n, m FROM T",
            Page {
                offset: 0,
                max_rows: 1000,
            },
        )
        .unwrap();
    // 40 cells / 2 columns = 20 rows.
    assert_eq!((r.row_count, r.next_offset), (20, Some(20)));
}

#[test]
fn unknown_table_lists_the_tables() {
    let q = engine(vec![sales(), regions()]);
    let err = q.query("SELECT * FROM Sales", Page::default()).unwrap_err();
    assert_eq!(err.code, QueryErrorCode::NoSuchTable);
    assert!(err.what.contains("\"Sales\""), "{err}");
    assert!(
        err.why.contains("\"Sales 2024\"") && err.why.contains("\"地区\""),
        "{err}"
    );
    assert!(err.fix.contains("double quotes"), "{err}");
}

#[test]
fn unknown_column_lists_the_columns_of_the_tables_used() {
    let q = engine(vec![sales(), regions()]);
    let err = q
        .query("SELECT Revenue FROM \"Sales 2024\"", Page::default())
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::NoSuchColumn);
    assert!(err.why.contains("\"Units\""), "{err}");
    assert!(!err.why.contains("区域"), "only the table used: {err}");
    // Double quotes name a column, never make a string.
    let err = q
        .query("SELECT \"Unit price\" FROM \"Sales 2024\"", Page::default())
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::NoSuchColumn);
}

#[test]
fn long_column_lists_are_bounded() {
    let header: Vec<QueryCell> = (0..100).map(|i| QueryCell::from(format!("c{i}"))).collect();
    let q = engine(vec![TableInput::new("Wide", vec![header])]);
    let err = q
        .query("SELECT nope FROM Wide", Page::default())
        .unwrap_err();
    assert!(err.why.contains("and 70 more"), "{err}");
    assert!(err.why.len() < 1000);
}

#[test]
fn syntax_and_empty_queries() {
    let q = engine(vec![sales()]);
    let err = q.query("SELEC * FROM x", Page::default()).unwrap_err();
    assert_eq!(err.code, QueryErrorCode::Syntax);
    let err = q.query("  -- nothing\n", Page::default()).unwrap_err();
    assert_eq!(err.code, QueryErrorCode::EmptyQuery);
    assert!(err.fix.contains("SELECT * FROM \"Sales 2024\""), "{err}");
}

#[test]
fn errors_serialize_with_the_fix_before_the_reason() {
    let q = engine(vec![sales()]);
    let err = q.query("SELECT * FROM nope", Page::default()).unwrap_err();
    let text = serde_json::to_string(&err).unwrap();
    assert!(
        text.starts_with("{\"code\":\"no_such_table\",\"what\":"),
        "{text}"
    );
    assert!(text.find("\"fix\"").unwrap() < text.find("\"why\"").unwrap());
}

#[test]
fn too_wide_input_is_refused() {
    let header: Vec<QueryCell> = (0..2001).map(|_| QueryCell::from("x")).collect();
    let err = QueryEngine::load(&[TableInput::new("W", vec![header])], Limits::default())
        .err()
        .unwrap();
    assert_eq!(err.code, QueryErrorCode::InvalidInput);
    assert!(err.what.contains("2001 columns"), "{err}");
}

#[test]
fn too_many_input_cells_are_refused() {
    let limits = Limits {
        max_input_cells: 3,
        ..Limits::default()
    };
    let err = QueryEngine::load(&[sales()], limits).err().unwrap();
    assert_eq!(err.code, QueryErrorCode::InvalidInput);
}

fn flags_and_numbers() -> QueryEngine {
    let flags = TableInput::new(
        "Flags",
        vec![vec!["f".into()], vec![true.into()], vec![false.into()]],
    );
    let nums = TableInput::new(
        "Nums",
        vec![vec!["n".into()], vec![1.0.into()], vec![5.0.into()]],
    );
    engine(vec![flags, nums])
}

#[test]
fn booleans_are_not_guessed_in_combined_selects() {
    let q = flags_and_numbers();
    let r = q
        .query(
            "SELECT f FROM Flags UNION ALL SELECT n FROM Nums",
            Page::default(),
        )
        .unwrap();
    assert_eq!(
        r.rows,
        vec![
            vec![json!(1)],
            vec![json!(0)],
            vec![json!(1)],
            vec![json!(5)]
        ]
    );
    assert!(
        r.notes.iter().any(|n| n.contains("1 and 0")),
        "{:?}",
        r.notes
    );
    // A plain SELECT of the column still gives true and false.
    let r = q.query("SELECT f FROM Flags", Page::default()).unwrap();
    assert_eq!(r.rows, vec![vec![json!(true)], vec![json!(false)]]);
    assert!(r.notes.is_empty(), "{:?}", r.notes);
}

#[test]
fn paging_without_order_by_gets_a_note() {
    let q = flags_and_numbers();
    let first = Page {
        offset: 0,
        max_rows: 1,
    };
    let r = q.query("SELECT n FROM Nums", first).unwrap();
    assert!(
        r.notes.iter().any(|n| n.contains("ORDER BY")),
        "{:?}",
        r.notes
    );
    let r = q.query("SELECT n FROM Nums ORDER BY n", first).unwrap();
    assert!(
        !r.notes.iter().any(|n| n.contains("no ORDER BY")),
        "{:?}",
        r.notes
    );
    // One page with everything needs no note.
    let r = q.query("SELECT n FROM Nums", Page::default()).unwrap();
    assert!(r.notes.is_empty(), "{:?}", r.notes);
}

#[test]
fn error_cells_are_counted_and_noted_in_aggregates() {
    let t = TableInput::new(
        "Prices",
        vec![
            vec!["item".into(), "price".into()],
            vec!["a".into(), 2.0.into()],
            vec!["b".into(), CellValue::Error(ErrorKind::Div0).into()],
            vec!["c".into(), CellValue::Error(ErrorKind::NA).into()],
        ],
    );
    let q = engine(vec![t]);
    assert_eq!(q.tables()[0].columns[1].errors, 2);
    assert_eq!(q.tables()[0].columns[0].errors, 0);
    let r = q
        .query("SELECT SUM(price) FROM Prices", Page::default())
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(2.0)]]);
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("\"price\"") && n.contains("2 error cells")),
        "{:?}",
        r.notes
    );
    let r = q
        .query(
            "SELECT SUM(price) FROM Prices WHERE typeof(price) <> 'text'",
            Page::default(),
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(2)]]);
    // No aggregate, no note.
    let r = q
        .query("SELECT price FROM Prices", Page::default())
        .unwrap();
    assert!(r.notes.is_empty(), "{:?}", r.notes);
}

#[test]
fn reserved_table_names_after_repeats() {
    let a = TableInput::new("SQLite", vec![vec!["v".into()], vec![1.0.into()]]);
    let b = TableInput::new("sqlite", vec![vec!["v".into()], vec![2.0.into()]]);
    let q = engine(vec![a, b]);
    assert_eq!(q.tables()[1].name, "_sqlite_2");
    assert_eq!(rows(&q, "SELECT v FROM _sqlite_2"), vec![vec![json!(2)]]);
}

#[test]
fn oversized_cells_and_rows_are_refused_at_load() {
    let limits = Limits {
        max_value_bytes: 1000,
        ..Limits::default()
    };
    // The header is sheet row 10, so rows[3] is row 13.
    let mut rows = vec![vec![QueryCell::from("a"), QueryCell::from("b")]];
    rows.extend((0..3).map(|_| vec![QueryCell::from("x"), QueryCell::from("y")]));
    rows[3][1] = QueryCell::from("z".repeat(1001));
    let table = TableInput::new("Notes", rows.clone()).with_first_row(9);
    let err = load_err(&[table], limits.clone());
    assert_eq!(err.code, QueryErrorCode::InvalidInput, "{err}");
    assert!(
        err.what.contains("B13") && err.what.contains("Notes"),
        "{err}"
    );
    assert!(err.why.contains("1000"), "{err}");

    // Each cell fits, the row does not.
    rows[3] = vec![
        QueryCell::from("z".repeat(600)),
        QueryCell::from("z".repeat(600)),
    ];
    let table = TableInput::new("Notes", rows).with_first_row(9);
    let err = load_err(&[table], limits);
    assert_eq!(err.code, QueryErrorCode::InvalidInput, "{err}");
    assert!(
        err.what.contains("row 13") && err.what.contains("Notes"),
        "{err}"
    );
}

#[test]
fn a_memory_budget_of_zero_is_refused() {
    let limits = Limits {
        memory_budget: 0,
        ..Limits::default()
    };
    let err = load_err(&[sales()], limits);
    assert_eq!(err.code, QueryErrorCode::InvalidInput, "{err}");
    assert!(err.what.contains("memory budget"), "{err}");
}

#[test]
fn errors_do_not_repeat_the_query() {
    let q = engine(vec![sales()]);
    let secret = "s".repeat(500);
    for sql in [
        format!("SELECT Revenue FROM \"Sales 2024\" WHERE Region = '{secret}'"),
        format!("SELEC '{secret}'"),
        format!("SELECT '{secret}' FROM nowhere"),
    ] {
        let err = q.query(&sql, Page::default()).unwrap_err();
        assert!(!err.what.contains(&secret), "{err}");
        assert!(!err.what.contains(" at offset "), "{err}");
        assert!(err.what.len() < 300, "{err}");
    }
    // SQLite's hint about single quotes is kept.
    let err = q
        .query(
            "SELECT * FROM \"Sales 2024\" WHERE Region = \"East\"",
            Page::default(),
        )
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::NoSuchColumn, "{err}");
    assert_eq!(err.what, "there is no column named \"East\"", "{err}");
    assert!(
        err.fix
            .starts_with("Should this be a string literal in single-quotes?"),
        "{err}"
    );
}

fn load_err(tables: &[TableInput], limits: Limits) -> carrysheet_query::QueryError {
    match QueryEngine::load(tables, limits) {
        Ok(_) => panic!("the load was not refused"),
        Err(e) => e,
    }
}

#[test]
fn long_names_are_cut_in_errors() {
    let q = engine(vec![sales()]);
    let long = "x".repeat(5000);
    for sql in [
        format!("SELECT * FROM \"{long}\""),
        format!("SELECT \"{long}\" FROM \"Sales 2024\""),
        format!("PRAGMA \"{long}\" = '{long}'"),
        format!("SELECT {long}(1)"),
        format!("SELECT * FROM \"Sales 2024\" WHERE Region = \"{long}\""),
        format!("SELECT \"a\"\"{long}\" FROM \"Sales 2024\""),
        // Names and tokens inside SQLite's own messages.
        format!("SELECT 1 {long} {long}"),
        format!("SELECT 1 FROM \"Sales 2024\" WHERE Region = '{long}"),
        format!("SELECT \"{long}\" FROM (SELECT 1 AS \"{long}\") a, (SELECT 1 AS \"{long}\") b"),
        format!("SELECT row_number() OVER {long} FROM \"Sales 2024\""),
        format!("SELECT Region FROM \"Sales 2024\" ORDER BY Region COLLATE {long}"),
    ] {
        let err = q.query(&sql, Page::default()).unwrap_err();
        assert!(err.what.len() < 200, "{} bytes: {err}", err.what.len());
        assert!(err.why.len() < 2000 && err.fix.len() < 2000, "{err}");
        assert!(!err.what.contains(&"x".repeat(65)), "{err}");
    }
    // Quotes in a name stay escaped.
    let err = q
        .query("SELECT \"a\"\"b\" FROM \"Sales 2024\"", Page::default())
        .unwrap_err();
    assert_eq!(err.what, "there is no column named \"a\"\"b\"", "{err}");
}

#[test]
fn errors_say_why_sqlite_refused() {
    let q = engine(vec![sales()]);
    for (sql, why) in [
        ("SELECT abs(-9223372036854775807 - 1)", "range"),
        ("SELECT json_extract('{\"a', '$')", "JSON"),
        (
            "WITH c(a, b) AS (SELECT 1) SELECT * FROM c",
            "number of values",
        ),
        ("SELECT 1 ORDER BY 5", "second result column"),
        (
            "SELECT count(*) FROM \"Sales 2024\" GROUP BY count(*)",
            "groups of rows",
        ),
    ] {
        let err = q.query(sql, Page::default()).unwrap_err();
        assert!(err.why.contains(why), "{sql}: {err}");
        assert_ne!(err.why, "SQLite could not run the query.", "{sql}: {err}");
    }
    let froms = vec!["\"Sales 2024\""; 65].join(", ");
    let err = q
        .query(&format!("SELECT 1 FROM {froms}"), Page::default())
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
    assert!(err.why.contains("64 tables"), "{err}");
    let mut ctes = vec!["t0 AS (SELECT 1 AS q)".to_owned()];
    for i in 1..10 {
        ctes.push(format!(
            "t{i} AS (SELECT a.q + b.q AS q FROM t{} a, t{} b)",
            i - 1,
            i - 1
        ));
    }
    let err = q
        .query(
            &format!("WITH {} SELECT * FROM t9", ctes.join(", ")),
            Page::default(),
        )
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
    assert!(err.why.contains("200 tables"), "{err}");
}

#[test]
fn only_max_min_and_json_extract_have_an_argument_limit() {
    let q = engine(vec![sales()]);
    let ones = |n: usize| vec!["1"; n].join(", ");
    for name in ["max", "min"] {
        let err = q
            .query(&format!("SELECT {name}({})", ones(33)), Page::default())
            .unwrap_err();
        assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
        assert!(err.what.starts_with(&format!("{name}()")), "{err}");
        assert!(err.fix.contains(&format!("{name}({name}(")), "{err}");
        q.query(&format!("SELECT {name}({})", ones(32)), Page::default())
            .unwrap();
    }
    let err = q
        .query(
            &format!(
                "SELECT json_extract('{{}}', {})",
                vec!["'$.a'"; 40].join(", ")
            ),
            Page::default(),
        )
        .unwrap_err();
    assert!(err.what.starts_with("json_extract()"), "{err}");
    for sql in [
        format!("SELECT json_object({})", vec!["'a', 1"; 30].join(", ")),
        format!("SELECT json_array({})", ones(60)),
        format!("SELECT printf('{}', {})", "%d".repeat(40), ones(40)),
        format!("SELECT coalesce({})", ones(60)),
    ] {
        q.query(&sql, Page::default()).unwrap();
    }
}

#[test]
fn input_names_are_cut_in_load_errors() {
    let name = "y".repeat(3000);
    let limits = Limits {
        max_value_bytes: 10,
        ..Limits::default()
    };
    let table = TableInput::new(
        name.as_str(),
        vec![vec!["a".into()], vec!["z".repeat(11).into()]],
    );
    let err = load_err(&[table], limits);
    assert!(err.what.len() < 200, "{err}");
}
