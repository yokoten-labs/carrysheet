//! The process-wide memory budget. Its own test binary with one test, because
//! the first engine in a process sets the budget for good.
#![cfg(not(target_family = "wasm"))]

use std::time::Duration;

use carrysheet_query::{Limits, Page, QueryCell, QueryEngine, QueryErrorCode, TableInput};
use serde_json::json;

const MIB: u64 = 1024 * 1024;

fn small() -> TableInput {
    TableInput::new("T", vec![vec![QueryCell::from("n")], vec![1.0.into()]])
}

/// A table of `rows` rows of 100-character text: about `rows / 10_000` MB.
fn big(rows: usize) -> TableInput {
    let text = "x".repeat(100);
    let mut data = vec![vec![QueryCell::from("t")]];
    data.extend((0..rows).map(|i| vec![QueryCell::from(format!("{i}{text}"))]));
    TableInput::new("Big", data)
}

fn limits(max_memory: u64) -> Limits {
    Limits {
        memory_budget: 256 * MIB,
        max_memory,
        max_steps: u64::MAX,
        max_time: Duration::from_secs(60),
        ..Limits::default()
    }
}

/// Sorts `rows` rows of 1000 bytes each: about `rows / 1000` MB in memory.
fn sort(rows: u32) -> String {
    format!(
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < {rows}) \
         SELECT x, zeroblob(1000) FROM c ORDER BY random()"
    )
}

#[test]
fn one_budget_for_the_process() {
    // A small engine first: it sets the budget (256 MiB) for the process.
    let small_engine = QueryEngine::load(&[small()], limits(64 * MIB)).unwrap();

    // A 40 MB sort fits in the budget and really runs.
    let r = small_engine
        .query(
            &sort(40_000),
            Page {
                offset: 0,
                max_rows: 1,
            },
        )
        .unwrap();
    assert_eq!(r.row_count, 1);

    // A runaway sort is stopped by the heap limit, not by steps or time.
    let endless = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) \
                   SELECT x, zeroblob(1000) FROM c ORDER BY random()";
    let err = small_engine.query(endless, Page::default()).unwrap_err();
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
    assert!(err.what.contains("memory"), "{err}");

    // Then a bigger engine: the limit set by the small one does not shrink
    // to its size, so 20 MB of data and an 80 MB sort both work.
    let big_engine = QueryEngine::load(&[big(200_000)], limits(96 * MIB)).unwrap();
    let r = big_engine
        .query(
            "SELECT COUNT(*), SUM(length(t)) > 20000000 FROM Big",
            Page::default(),
        )
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(200_000), json!(1)]]);
    let r = big_engine
        .query(
            &sort(80_000),
            Page {
                offset: 0,
                max_rows: 1,
            },
        )
        .unwrap();
    assert_eq!(r.row_count, 1);
    // The small engine still works next to it.
    let r = small_engine
        .query("SELECT n FROM T", Page::default())
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);

    // Asking for more than is left is refused at load, with a clear error.
    let err = QueryEngine::load(&[small()], limits(230 * MIB))
        .err()
        .expect("over budget");
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");
    assert!(err.why.contains("256 MiB"), "{err}");

    // A later engine's own budget is ignored: the first one set it for good.
    let err = QueryEngine::load(
        &[small()],
        Limits {
            memory_budget: 4096 * MIB,
            ..limits(230 * MIB)
        },
    )
    .err()
    .expect("budget is fixed");
    assert_eq!(err.code, QueryErrorCode::TooLarge, "{err}");

    // Dropping the big engine gives its share back.
    drop(big_engine);
    let again = QueryEngine::load(&[small()], limits(230 * MIB)).unwrap();
    let r = again.query("SELECT n FROM T", Page::default()).unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);
}
