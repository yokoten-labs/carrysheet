//! Hostile SQL: refused with a clear error, or stopped by a limit.
#![cfg(not(target_family = "wasm"))]

use std::time::{Duration, Instant};

use carrysheet_query::{
    Limits, Page, QueryCell, QueryEngine, QueryError, QueryErrorCode, TableInput,
};
use serde_json::json;

fn numbers(name: &str, n: u32) -> TableInput {
    let mut rows = vec![vec![QueryCell::from("n")]];
    rows.extend((1..=n).map(|i| vec![QueryCell::from(f64::from(i))]));
    TableInput::new(name, rows)
}

fn engine_with(limits: Limits) -> QueryEngine {
    QueryEngine::load(&[numbers("Data", 1000)], limits).expect("load")
}

fn engine() -> QueryEngine {
    engine_with(Limits::default())
}

fn refused(q: &QueryEngine, sql: &str) -> QueryError {
    match q.query(sql, Page::default()) {
        Ok(r) => panic!("{sql} was not refused: {r:?}"),
        Err(e) => e,
    }
}

fn assert_not_allowed(q: &QueryEngine, sql: &str, names: &str) {
    let err = refused(q, sql);
    assert_eq!(err.code, QueryErrorCode::NotAllowed, "{sql}: {err}");
    assert!(err.what.contains(names), "{sql}: {err}");
    assert!(err.why.contains("read-only"), "{sql}: {err}");
    assert!(err.fix.contains("SELECT"), "{sql}: {err}");
}

/// The data is unchanged after every refused statement.
fn assert_intact(q: &QueryEngine) {
    let r = q
        .query("SELECT COUNT(*), SUM(n) FROM Data", Page::default())
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1000), json!(500_500)]]);
}

#[test]
fn attach_and_detach_are_refused() {
    let q = engine();
    assert_not_allowed(&q, "ATTACH DATABASE ':memory:' AS other", "ATTACH");
    assert_not_allowed(&q, "ATTACH 'evil.db' AS evil", "ATTACH");
    assert_not_allowed(&q, "DETACH DATABASE main", "DETACH");
    assert!(!std::path::Path::new("evil.db").exists());
}

#[test]
fn load_extension_is_refused() {
    let q = engine();
    for sql in [
        "SELECT load_extension('/tmp/evil.so')",
        "SELECT LOAD_EXTENSION('evil', 'init')",
    ] {
        let err = refused(&q, sql);
        assert_eq!(err.code, QueryErrorCode::NotAllowed, "{sql}: {err}");
        assert!(err.what.contains("load_extension()"), "{sql}: {err}");
    }
}

#[test]
fn writes_are_refused() {
    let q = engine();
    assert_not_allowed(&q, "CREATE TABLE t (x)", "CREATE");
    assert_not_allowed(&q, "CREATE TEMP TABLE t (x)", "CREATE");
    assert_not_allowed(&q, "CREATE VIEW v AS SELECT 1", "CREATE");
    assert_not_allowed(&q, "CREATE INDEX i ON Data (n)", "CREATE");
    assert_not_allowed(&q, "INSERT INTO Data VALUES (1)", "INSERT");
    assert_not_allowed(&q, "UPDATE Data SET n = 0", "UPDATE");
    assert_not_allowed(&q, "DELETE FROM Data", "DELETE");
    assert_not_allowed(&q, "DROP TABLE Data", "DROP");
    assert_not_allowed(&q, "ALTER TABLE Data ADD COLUMN y", "ALTER");
    assert_not_allowed(&q, "BEGIN", "BEGIN");
    assert_not_allowed(&q, "SAVEPOINT s", "SAVEPOINT");
    assert_not_allowed(&q, "VACUUM", "VACUUM");
    assert_not_allowed(&q, "REINDEX", "REINDEX");
    assert_not_allowed(&q, "ANALYZE", "ANALYZE");
    assert_not_allowed(&q, "CREATE VIRTUAL TABLE f USING fts5(x)", "CREATE");
    // Writes hidden after WITH get past the first-word check; the authorizer
    // refuses them.
    assert_not_allowed(
        &q,
        "WITH x AS (SELECT 1) INSERT INTO Data SELECT * FROM x",
        "INSERT INTO Data",
    );
    assert_not_allowed(
        &q,
        "WITH x AS (SELECT 1) DELETE FROM Data",
        "DELETE FROM Data",
    );
    assert_not_allowed(
        &q,
        "WITH x AS (SELECT 1) UPDATE Data SET n = 0",
        "UPDATE Data",
    );
    assert_intact(&q);
}

#[test]
fn writing_pragmas_are_refused() {
    let q = engine();
    assert_not_allowed(&q, "PRAGMA writable_schema = ON", "PRAGMA writable_schema");
    assert_not_allowed(&q, "PRAGMA writable_schema", "PRAGMA writable_schema");
    assert_not_allowed(&q, "PRAGMA query_only = OFF", "PRAGMA query_only");
    assert_not_allowed(&q, "PRAGMA journal_mode = OFF", "PRAGMA journal_mode");
    assert_not_allowed(&q, "PRAGMA trusted_schema = ON", "PRAGMA trusted_schema");
    assert_intact(&q);
}

#[test]
fn a_second_statement_is_refused() {
    let q = engine();
    let err = refused(&q, "SELECT 1; SELECT 2");
    assert_eq!(err.code, QueryErrorCode::MultipleStatements, "{err}");
    let err = refused(&q, "SELECT 1; DROP TABLE Data");
    assert!(
        matches!(
            err.code,
            QueryErrorCode::NotAllowed | QueryErrorCode::MultipleStatements
        ),
        "{err}"
    );
    assert_intact(&q);
    // A trailing semicolon is fine.
    assert!(q.query("SELECT 1;", Page::default()).is_ok());
}

#[test]
fn huge_cross_join_is_stopped_by_the_step_limit() {
    let q = engine_with(Limits {
        max_steps: 2_000_000,
        ..Limits::default()
    });
    let started = Instant::now();
    // 1000^3 = a billion rows.
    let err = refused(&q, "SELECT COUNT(*) FROM Data a, Data b, Data c");
    assert_eq!(err.code, QueryErrorCode::StepLimit, "{err}");
    assert!(err.fix.contains("WHERE"), "{err}");
    // Generous: queries run one at a time, so this may wait for other tests.
    assert!(started.elapsed() < Duration::from_secs(30));
    // The engine still works afterwards.
    assert_intact(&q);
}

#[test]
fn default_limits_stop_a_huge_cross_join() {
    let q = engine();
    let err = refused(&q, "SELECT COUNT(*) FROM Data a, Data b, Data c, Data d");
    assert!(
        matches!(
            err.code,
            QueryErrorCode::StepLimit | QueryErrorCode::TimeLimit
        ),
        "{err}"
    );
}

#[test]
fn slow_query_is_stopped_by_the_time_limit() {
    let q = engine_with(Limits {
        max_steps: u64::MAX,
        max_time: Duration::from_millis(50),
        ..Limits::default()
    });
    let started = Instant::now();
    let err = refused(&q, "SELECT COUNT(*) FROM Data a, Data b, Data c");
    assert_eq!(err.code, QueryErrorCode::TimeLimit, "{err}");
    // Generous: queries run one at a time, so this may wait for other tests.
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn endless_recursion_is_stopped() {
    let q = engine_with(Limits {
        max_steps: 1_000_000,
        ..Limits::default()
    });
    let err = refused(
        &q,
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT MAX(x) FROM c",
    );
    assert_eq!(err.code, QueryErrorCode::StepLimit, "{err}");
    // A bounded recursive query is fine.
    let r = q
        .query(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 10) SELECT SUM(x) FROM c",
            Page::default(),
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(55)]]);
}

#[test]
fn an_endless_result_is_paged_not_collected() {
    let q = engine();
    let r = q
        .query(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT x FROM c",
            Page {
                offset: 0,
                max_rows: 5,
            },
        )
        .unwrap();
    assert_eq!(r.row_count, 5);
    assert_eq!(r.next_offset, Some(5));
}

#[test]
fn huge_values_are_refused() {
    let q = engine_with(Limits {
        max_value_bytes: 1_000_000,
        ..Limits::default()
    });
    for sql in [
        "SELECT zeroblob(2000000)",
        "SELECT randomblob(2000000)",
        "SELECT printf('%.*c', 2000000, 'x')",
        "SELECT replace(hex(zeroblob(400000)), '0', 'aaaa')",
    ] {
        let err = refused(&q, sql);
        assert_eq!(err.code, QueryErrorCode::TooLarge, "{sql}: {err}");
    }
}

#[test]
fn long_query_text_is_refused() {
    let q = engine();
    let sql = format!("SELECT {}1", "1 + ".repeat(30_000));
    let err = refused(&q, &sql);
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
}

#[test]
fn deep_expressions_are_refused_without_crashing() {
    let q = engine();
    let sql = format!("SELECT {}1{}", "(".repeat(5000), ")".repeat(5000));
    let err = refused(&q, &sql);
    assert_ne!(err.code, QueryErrorCode::Internal, "{err}");
}

#[test]
fn schema_can_be_read_but_not_written() {
    let q = engine();
    let r = q
        .query(
            "SELECT name FROM sqlite_schema WHERE type = 'table'",
            Page::default(),
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!("Data")]]);
    assert_not_allowed(
        &q,
        "UPDATE sqlite_schema SET sql = 'x' WHERE name = 'Data'",
        "UPDATE",
    );
    assert_intact(&q);
}

#[test]
fn double_quoted_strings_are_not_text() {
    let q = engine();
    // With SQLite's legacy rule this would return the text "nope" for every row.
    let err = refused(&q, "SELECT \"nope\" FROM Data");
    assert_eq!(err.code, QueryErrorCode::NoSuchColumn, "{err}");
}

/// Runs `sql` and returns how long it took, failing past a few seconds.
fn timed(
    q: &QueryEngine,
    sql: &str,
) -> (Result<carrysheet_query::QueryResult, QueryError>, Duration) {
    let started = Instant::now();
    let result = q.query(sql, Page::default());
    (result, started.elapsed())
}

#[test]
fn quadratic_string_functions_run_in_linear_time() {
    // Each of these costs about 10^11 comparisons in SQLite's own versions:
    // hours in one call the progress handler cannot interrupt.
    let q = engine();
    let a = "printf('%.*c', 200000, 'a')";
    let b = "printf('%.*c', 200000, 'b')";
    let cases = [
        (format!("SELECT length(trim({a}, {b} || 'a'))"), json!(0)),
        (
            format!("SELECT length(ltrim({a} || 'x', {b} || 'a'))"),
            json!(1),
        ),
        (
            format!("SELECT length(rtrim('x' || {a}, {b} || 'a'))"),
            json!(1),
        ),
        (
            format!("SELECT instr({a}, printf('%.*c', 100000, 'a') || 'b')"),
            json!(0),
        ),
        (
            format!("SELECT instr({a} || 'b', printf('%.*c', 100000, 'a') || 'b')"),
            json!(100_001),
        ),
        (
            format!("SELECT length(replace({a}, printf('%.*c', 100000, 'a') || 'b', 'x'))"),
            json!(200_000),
        ),
    ];
    for (sql, expected) in cases {
        let (result, took) = timed(&q, &sql);
        let result = result.unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert_eq!(result.rows, vec![vec![expected]], "{sql}");
        assert!(took < Duration::from_secs(10), "{sql} took {took:?}");
    }
}

#[test]
fn values_are_limited_to_256_kib_by_default() {
    let q = engine();
    let err = refused(&q, "SELECT zeroblob(262145)");
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
    let err = refused(&q, "SELECT replace(printf('%.*c', 150000, 'a'), 'a', 'bb')");
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
}

#[test]
fn long_like_patterns_are_refused() {
    let q = engine();
    let err = refused(
        &q,
        "SELECT printf('%.*c', 100000, 'a') LIKE printf('%.*c', 101, '%')",
    );
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
    assert!(err.what.contains("LIKE"), "{err}");
    let err = refused(&q, "SELECT 'a' GLOB printf('%.*c', 101, '*')");
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
    // Up to 100 characters is fine.
    let r = q
        .query(
            "SELECT 'abc' LIKE printf('%.*c', 100, '%')",
            Page::default(),
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);
}

#[test]
fn thousands_of_statements_do_not_recurse() {
    // rusqlite's Connection::prepare checks for a second statement by
    // preparing the rest recursively; 11,000 statements would need a deep
    // stack. A 256 KiB stack proves the check does not recurse.
    let sql = "SELECT 1;".repeat(11_000);
    let err = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let q = QueryEngine::load(&[numbers("Data", 3)], Limits::default()).unwrap();
            q.query(&sql, Page::default()).unwrap_err()
        })
        .unwrap()
        .join()
        .expect("the query thread crashed");
    assert_eq!(err.code, QueryErrorCode::MultipleStatements, "{err}");
}

#[test]
fn functions_off_the_list_are_refused() {
    let q = engine();
    for (sql, name) in [
        ("SELECT unhex('41')", "unhex"),
        ("SELECT soundex('a')", "soundex"),
        ("SELECT json_patch('{}', '{}')", "json_patch"),
        ("SELECT json_set('{}', '$.a', 1)", "json_set"),
        ("SELECT json_insert('{}', '$.a', 1)", "json_insert"),
        ("SELECT json_replace('{}', '$.a', 1)", "json_replace"),
        ("SELECT json_remove('{\"a\":1}', '$.a')", "json_remove"),
        ("SELECT jsonb('{}')", "jsonb"),
        (
            "SELECT max(n) FROM Data WHERE sqlite_compileoption_used('DQS=0')",
            "sqlite_compileoption_used",
        ),
    ] {
        let err = refused(&q, sql);
        assert_eq!(err.code, QueryErrorCode::NotAllowed, "{sql}: {err}");
        assert!(err.what.contains(&format!("{name}()")), "{sql}: {err}");
        assert!(err.fix.contains("allowed function"), "{sql}: {err}");
    }
    let err = refused(&q, "SELECT * FROM json_tree('{\"a\":1}')");
    assert_eq!(err.code, QueryErrorCode::NotAllowed, "{err}");
    assert!(err.what.contains("json_tree"), "{err}");
    for sql in [
        "SELECT * FROM pragma_function_list",
        "SELECT count(*) FROM json_tree('[1, [2]]')",
        "SELECT count(*) FROM pragma_function_list",
    ] {
        let err = refused(&q, sql);
        assert_eq!(err.code, QueryErrorCode::NotAllowed, "{sql}: {err}");
    }
    assert_intact(&q);

    // Allowed ones still work, upper case or not.
    let r = q
        .query(
            "SELECT JSON_EXTRACT('{\"a\":2}', '$.a') + SUM(n), \
             (SELECT group_concat(value) FROM json_each('[1,2]')), \
             '{\"b\":3}' ->> '$.b' FROM Data",
            Page::default(),
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(500_502), json!("1,2"), json!(3)]]);
    // Counting the rows of a WITH table reads it without naming a column.
    let r = q
        .query(
            "WITH Data(x) AS (SELECT 1 UNION ALL SELECT 2) SELECT count(*) FROM Data",
            Page::default(),
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(2)]]);
}
