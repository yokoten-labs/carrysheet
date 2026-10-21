//! Time limits that hold up against slow function calls and other queries.
#![cfg(not(target_family = "wasm"))]

use std::time::{Duration, Instant};

use carrysheet_query::{Limits, Page, QueryCell, QueryEngine, QueryErrorCode, TableInput};

const SLACK: Duration = Duration::from_millis(500);

#[test]
fn long_like_patterns_over_large_values_stay_in_time() {
    // 120 rows of text as large as a value may be (256 KiB with the row's
    // own bytes). Each LIKE call tries the 100-character pattern at every
    // position, which the progress handler cannot interrupt; the call is
    // short because the pattern is capped, so the limit holds.
    let limits = Limits {
        max_time: Duration::from_secs(1),
        max_steps: u64::MAX,
        ..Limits::default()
    };
    let text = "a".repeat(limits.max_value_bytes - 9);
    let mut rows = vec![vec![QueryCell::from("t")]];
    rows.extend((0..120).map(|_| vec![QueryCell::from(text.clone())]));
    let q = QueryEngine::load(&[TableInput::new("Big", rows)], limits.clone()).unwrap();
    let pattern = format!("%{}b", "a".repeat(98));
    assert_eq!(pattern.len(), 100);
    let sql = format!(
        "SELECT count(*) FROM Big WHERE t LIKE '{pattern}' OR t GLOB '*{}b'",
        "a".repeat(98)
    );
    let started = Instant::now();
    let result = q.query(&sql, Page::default());
    let took = started.elapsed();
    let err = result.expect_err("the query should run out of time");
    assert_eq!(err.code, QueryErrorCode::TimeLimit, "{err}");
    assert!(took <= limits.max_time + SLACK, "took {took:?}");
}

#[test]
fn waiting_for_another_query_counts_toward_the_time_limit() {
    let slow = QueryEngine::load(
        &[TableInput::new(
            "T",
            vec![vec!["n".into()], vec![1.0.into()]],
        )],
        Limits {
            max_time: Duration::from_secs(3),
            max_steps: u64::MAX,
            ..Limits::default()
        },
    )
    .unwrap();
    let quick_limits = Limits {
        max_time: Duration::from_millis(500),
        ..Limits::default()
    };
    let quick = QueryEngine::load(
        &[TableInput::new(
            "T",
            vec![vec!["n".into()], vec![1.0.into()]],
        )],
        quick_limits.clone(),
    )
    .unwrap();
    let busy = std::thread::spawn(move || {
        slow.query(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) \
             SELECT count(*) FROM c",
            Page::default(),
        )
    });
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let err = quick.query("SELECT n FROM T", Page::default()).unwrap_err();
    let took = started.elapsed();
    assert_eq!(err.code, QueryErrorCode::TimeLimit, "{err}");
    assert!(err.why.contains("Another query"), "{err}");
    assert!(took <= quick_limits.max_time + SLACK, "took {took:?}");
    let busy = busy.join().unwrap().unwrap_err();
    assert_eq!(busy.code, QueryErrorCode::TimeLimit, "{busy}");
    // Free again: the quick query runs.
    assert!(quick.query("SELECT n FROM T", Page::default()).is_ok());
}
