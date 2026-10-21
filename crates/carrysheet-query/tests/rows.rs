//! Work inside one row: queries that SQLite cannot interrupt between two
//! checks of the time limit. Each must stop within 0.3 s of a 1 s limit, or
//! be refused before it runs. In this process SQLite checks the time only
//! between rows, so a query of many rows may also run one more row past the
//! limit: the cost model allows a row up to a second of work, and how long
//! that row takes depends on the machine. One test, so peak memory is this
//! query's own.
#![cfg(not(target_family = "wasm"))]

use std::time::{Duration, Instant};

use carrysheet_query::{Limits, Page, QueryCell, QueryEngine, QueryErrorCode, TableInput};

const SLACK: Duration = Duration::from_millis(300);
/// Past one more row: the timer's wake-up and the step that ends the query.
const ROW_SLACK: Duration = Duration::from_millis(100);

/// Queries of many rows, and the same query on one of their rows: the
/// time limit may let one more row run, as long as that one row took.
const ONE_ROW_OF: &[(&str, &str)] = &[
    (
        "max(t COLLATE NOCASE, t x 31) x 49, 10 rows",
        "max(t COLLATE NOCASE, t x 31) x 49",
    ),
    (
        "json_extract(j, path x 31) x 49, 10 rows",
        "json_extract(j, path x 31) x 49",
    ),
];
const MIB: u64 = 1024 * 1024;

fn one_row(name: &str, columns: &[(&str, String)]) -> TableInput {
    let header = columns.iter().map(|(c, _)| QueryCell::from(*c)).collect();
    let row = columns
        .iter()
        .map(|(_, v)| QueryCell::from(v.clone()))
        .collect();
    TableInput::new(name, vec![header, row])
}

/// Peak resident memory since the last reset, in MiB (Linux only).
fn peak_mib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024)
}

fn reset_peak() {
    // "5" resets VmHWM to the current size (Linux 4.0 and later).
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

fn rep(item: &str, n: usize, sep: &str) -> String {
    vec![item; n].join(sep)
}

#[test]
fn one_row_cannot_run_past_the_time_limit() {
    let limits = Limits {
        max_time: Duration::from_secs(1),
        max_steps: u64::MAX,
        memory_budget: 256 * MIB,
        max_memory: 128 * MIB,
        // Room for the reviewer's longest queries.
        max_sql_bytes: 200_000,
        ..Limits::default()
    };
    let big = "a".repeat(262_135);
    let half = "a".repeat(131_000);
    let doc = format!(
        "{{{}}}",
        (0..21_840)
            .map(|i| format!("\"k{i:05}\":0"))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(doc.len() <= 262_135, "{}", doc.len());
    let rows = |name: &str, columns: &[(&str, &str)], n: usize| {
        let mut data = vec![columns.iter().map(|(c, _)| QueryCell::from(*c)).collect()];
        data.extend((0..n).map(|_| columns.iter().map(|(_, v)| QueryCell::from(*v)).collect()));
        TableInput::new(name, data)
    };
    let many = rows("Many", &[("t", &big)], 10);
    let docs = rows("Docs", &[("j", &doc), ("x", "$.zz")], 10);
    let tables = [
        many,
        docs,
        one_row("Big", &[("t", big)]),
        one_row(
            "Pair",
            &[("u", format!("{half}x")), ("v", format!("{half}y"))],
        ),
        one_row("Doc", &[("j", doc), ("x", "$.zz".to_owned())]),
    ];
    let q = QueryEngine::load(&tables, limits.clone()).unwrap();

    let p = format!("'%{}b'", "a".repeat(98));
    let k1999 = rep("k", 1999, ",");
    let k20 = rep("k", 20, "+");
    let k2_20 = rep("k2", 20, ",");
    let l30 = |name: &str| rep(name, 30, "+");
    let nocase_max = |args: usize| format!("max(t COLLATE NOCASE, {})", rep("t", args, ", "));
    let json_paths = |args: usize| format!("json_extract(j, {})", rep("x", args, ", "));
    let like = format!("t LIKE {p}");
    let cases: Vec<(&str, String)> = vec![
        (
            "1,999 copies of a LIKE (subquery)",
            format!("SELECT {k1999} FROM (SELECT t LIKE {p} AS k FROM Big)"),
        ),
        (
            "1,999 copies of a LIKE (WITH)",
            format!("WITH s AS (SELECT t LIKE {p} AS k FROM Big) SELECT {k1999} FROM s"),
        ),
        (
            "the same, AS MATERIALIZED",
            format!(
                "WITH s AS MATERIALIZED (SELECT t LIKE {p} AS k FROM Big) SELECT {k1999} FROM s"
            ),
        ),
        (
            "two levels: 20 x 20 LIKEs",
            format!(
                "SELECT {k2_20} FROM (SELECT {k20} AS k2 FROM (SELECT t LIKE {p} AS k FROM Big))"
            ),
        ),
        (
            "four levels x 30 over length(t)",
            format!(
                "SELECT {} FROM (SELECT {} AS k3 FROM (SELECT {} AS k2 FROM (SELECT {} AS k1 FROM (SELECT length(t) AS k0 FROM Big))))",
                l30("k3"),
                l30("k2"),
                l30("k1"),
                l30("k0")
            ),
        ),
        (
            "u COLLATE NOCASE IN (v x 49,960)",
            format!(
                "SELECT u COLLATE NOCASE IN ({}) FROM Pair",
                rep("v", 49_960, ",")
            ),
        ),
        (
            "max(t COLLATE NOCASE, t x 999) x 49",
            format!("SELECT {} FROM Big", rep(&nocase_max(999), 49, ", ")),
        ),
        (
            "max(t COLLATE NOCASE, t x 31) x 49",
            format!("SELECT {} FROM Big", rep(&nocase_max(31), 49, ", ")),
        ),
        (
            "json_extract(j, path x 999) x 49",
            format!("SELECT {} FROM Doc", rep(&json_paths(999), 49, ", ")),
        ),
        (
            "json_extract(j, path x 31) x 49",
            format!("SELECT {} FROM Doc", rep(&json_paths(31), 49, ", ")),
        ),
        (
            "max(t COLLATE NOCASE, t x 31) x 49, 10 rows",
            format!("SELECT {} FROM Many", rep(&nocase_max(31), 49, ", ")),
        ),
        (
            "json_extract(j, path x 31) x 49, 10 rows",
            format!("SELECT {} FROM Docs", rep(&json_paths(31), 49, ", ")),
        ),
        (
            "6 LIKEs in one row",
            format!("SELECT count(*) FROM Big WHERE {}", rep(&like, 6, " OR ")),
        ),
        (
            "300 LIKEs in one row",
            format!("SELECT count(*) FROM Big WHERE {}", rep(&like, 300, " OR ")),
        ),
        (
            "251 length(upper(t)) in one row",
            format!(
                "SELECT count(*) FROM Big WHERE {}",
                rep("length(upper(t)) = 0", 251, " OR ")
            ),
        ),
        (
            "1,000 length(upper(t)) columns",
            format!("SELECT {} FROM Big", rep("length(upper(t))", 1000, ", ")),
        ),
    ];
    let mut failures = Vec::new();
    let mut times = std::collections::HashMap::new();
    for (name, sql) in &cases {
        assert!(
            sql.len() <= limits.max_sql_bytes,
            "{name}: {} bytes",
            sql.len()
        );
        reset_peak();
        let before = peak_mib();
        let started = Instant::now();
        let result = q.query(sql, Page::default());
        let took = started.elapsed();
        let peak = peak_mib();
        let outcome = match &result {
            Ok(_) => "ran".to_owned(),
            Err(e) => format!("{} ({})", e.code, e.what),
        };
        eprintln!(
            "{name:40} {took:>10.1?}  peak {:?} MiB (from {:?})  {outcome}",
            peak, before
        );
        if let Err(e) = &result
            && !matches!(e.code, QueryErrorCode::TimeLimit | QueryErrorCode::TooLarge)
        {
            failures.push(format!("{name}: unexpected {e}"));
        }
        times.insert(*name, took);
        // A query of many rows may run one more row, measured on this
        // machine by the same query on one row (run before it).
        let one_row = ONE_ROW_OF
            .iter()
            .find(|(many, _)| many == name)
            .map(|(_, one)| times[one] + ROW_SLACK);
        let slack = one_row.map_or(SLACK, |row| row.max(SLACK));
        if took > limits.max_time + slack {
            failures.push(format!(
                "{name}: took {took:?}, allowed {slack:?} past the limit"
            ));
        }
        // Compiling may use what is left of the memory budget.
        if let (Some(peak), Some(before)) = (peak, before)
            && peak.saturating_sub(before) > limits.memory_budget / MIB + 64
        {
            failures.push(format!("{name}: peak memory grew {} MiB", peak - before));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
