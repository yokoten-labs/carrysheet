//! Queries in a worker process: every slow query the reviews found stops
//! within 0.5 s of a 1 s time limit (or is refused before it runs), the
//! worker stays under its memory cap, a killed worker comes back, the usual
//! queries still run at about the same speed, and no process is left
//! behind. Fake workers check what the parent does when a worker misbehaves:
//! answers garbage, never reads, reloads slowly, dies, or outlives its
//! parent.
//!
//! The test binary is its own worker: `harness = false`, and `main` serves
//! queries when it gets `__query-worker`, plays a fake worker with
//! `__fake-worker`, and plays a parent with `__parent`.
#![cfg(not(target_family = "wasm"))]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use carrysheet_query::{
    Limits, Page, QueryCell, QueryEngine, QueryErrorCode, TableInput, Worker, serve_worker,
};

const WORKER_ARG: &str = "__query-worker";
/// One test: `Err` says what failed.
type Test = fn() -> Result<(), String>;
const MIB: u64 = 1024 * 1024;
/// How long past the time limit a query may take, kill included.
const SLACK: Duration = Duration::from_millis(500);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some(WORKER_ARG) => return serve_worker(),
        Some(FAKE_ARG) => return fake_worker(&args[2..]),
        Some(PARENT_ARG) => return fake_parent(),
        _ => {}
    }
    // The memory budget is set once per process, by the first engine: set
    // it here at the default, 512 MiB, whatever test runs first.
    drop(QueryEngine::load(&[], Limits::default()).expect("the first engine"));
    // `cargo test -- <filter>` passes a filter; run everything anyway, it
    // is one test.
    let tests: [(&str, Test); 12] = [
        (
            "slow_queries_stop_at_the_time_limit",
            slow_queries_stop_at_the_time_limit,
        ),
        ("a_killed_worker_comes_back", a_killed_worker_comes_back),
        ("usual_queries_run_as_fast", usual_queries_run_as_fast),
        (
            "a_slow_reload_has_its_own_time",
            a_slow_reload_has_its_own_time,
        ),
        (
            "bad_replies_are_internal_errors",
            bad_replies_are_internal_errors,
        ),
        (
            "an_outside_kill_is_not_memory",
            an_outside_kill_is_not_memory,
        ),
        (
            "a_worker_that_never_reads_is_killed",
            a_worker_that_never_reads_is_killed,
        ),
        (
            "worker_engines_share_the_budget",
            worker_engines_share_the_budget,
        ),
        (
            "the_worker_dies_with_its_parent",
            the_worker_dies_with_its_parent,
        ),
        ("stderr_is_kept_and_scrubbed", stderr_is_kept_and_scrubbed),
        ("a_query_frame_has_a_cap", a_query_frame_has_a_cap),
        ("no_process_is_left_behind", no_process_is_left_behind),
    ];
    let mut failed = 0;
    for (name, test) in tests {
        let started = Instant::now();
        match std::panic::catch_unwind(test) {
            Ok(Ok(())) => eprintln!("test {name} ... ok ({:.1?})", started.elapsed()),
            Ok(Err(why)) => {
                failed += 1;
                eprintln!("test {name} ... FAILED\n{why}");
            }
            Err(_) => {
                failed += 1;
                eprintln!("test {name} ... FAILED (panicked)");
            }
        }
    }
    if failed == 0 {
        eprintln!("test result: ok. {} passed", tests.len());
        ExitCode::SUCCESS
    } else {
        eprintln!("test result: FAILED. {failed} failed");
        ExitCode::FAILURE
    }
}

fn worker() -> Worker {
    Worker::current_exe([WORKER_ARG]).expect("the test binary")
}

fn limits() -> Limits {
    Limits {
        max_time: Duration::from_secs(1),
        max_steps: u64::MAX,
        memory_budget: 256 * MIB,
        max_memory: 128 * MIB,
        max_sql_bytes: 200_000,
        ..Limits::default()
    }
}

fn rep(item: &str, n: usize, sep: &str) -> String {
    vec![item; n].join(sep)
}

fn rows(name: &str, columns: &[(&str, &str)], n: usize) -> TableInput {
    let mut data = vec![columns.iter().map(|(c, _)| QueryCell::from(*c)).collect()];
    data.extend((0..n).map(|_| columns.iter().map(|(_, v)| QueryCell::from(*v)).collect()));
    TableInput::new(name, data)
}

/// Resident memory of a process now and at its peak, in MiB (Linux only).
fn memory_mib(pid: u32) -> Option<(u64, u64)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |name: &str| -> Option<u64> {
        let line = status.lines().find(|l| l.starts_with(name))?;
        Some(line.split_whitespace().nth(1)?.parse::<u64>().ok()? / 1024)
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

/// Watches a worker's resident memory until dropped; the peak it saw.
struct Watch {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    fn new(pid: Option<u32>) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let thread = pid.map(|pid| {
            let (stop, peak) = (Arc::clone(&stop), Arc::clone(&peak));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Some((_, hwm)) = memory_mib(pid) {
                        peak.fetch_max(hwm, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        });
        Watch { stop, peak, thread }
    }

    fn peak(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.peak.load(Ordering::Relaxed)
    }
}

/// The slow queries of review rounds 3 and 4.
fn probes() -> Vec<(&'static str, String)> {
    let p = format!("'%{}b'", "a".repeat(98));
    let k1999 = rep("k", 1999, ",");
    let k20 = rep("k", 20, "+");
    let k2_20 = rep("k2", 20, ",");
    let l30 = |name: &str| rep(name, 30, "+");
    let nocase_max = |args: usize| format!("max(t COLLATE NOCASE, {})", rep("t", args, ", "));
    let json_paths = |args: usize| format!("json_extract(j, {})", rep("x", args, ", "));
    let like = format!("t LIKE {p}");
    let digits = "WITH s AS MATERIALIZED (SELECT printf('%.*c', 262000, '1') AS d)";
    let ten_digits = "WITH s AS MATERIALIZED (SELECT printf('%.*c', 262000, '1') AS d FROM Many)";
    let sum = |m: usize| format!("({})", rep("d", m, "+"));
    let nest = |levels: usize, m: usize| {
        let mut sql = "SELECT 1 AS k0".to_owned();
        for i in 1..=levels {
            sql = format!(
                "SELECT {} AS k{i} FROM ({sql})",
                rep(&format!("k{}", i - 1), m, "+")
            );
        }
        sql
    };
    let nest_cte = |levels: usize, m: usize| {
        let mut ctes = vec!["c0 AS (SELECT 1 AS k)".to_owned()];
        for i in 1..=levels {
            ctes.push(format!(
                "c{i} AS (SELECT {} AS k FROM c{})",
                rep("k", m, "+"),
                i - 1
            ));
        }
        format!("WITH {} SELECT k FROM c{levels}", ctes.join(", "))
    };
    let counter = |n: usize| {
        format!("WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM r WHERE i < {n})")
    };
    vec![
        // Round 3 and 4: copies of a subquery's column.
        (
            "1,999 copies of a LIKE (subquery)",
            format!("SELECT {k1999} FROM (SELECT t LIKE {p} AS k FROM Big)"),
        ),
        (
            "1,999 copies of a LIKE (WITH)",
            format!("WITH s AS (SELECT t LIKE {p} AS k FROM Big) SELECT {k1999} FROM s"),
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
            "max(t COLLATE NOCASE, t x 31) x 49, 10 rows",
            format!("SELECT {} FROM Many", rep(&nocase_max(31), 49, ", ")),
        ),
        (
            "json_extract(j, path x 999) x 49",
            format!("SELECT {} FROM Doc", rep(&json_paths(999), 49, ", ")),
        ),
        (
            "json_extract(j, path x 31) x 49, 10 rows",
            format!("SELECT {} FROM Docs", rep(&json_paths(31), 49, ", ")),
        ),
        (
            "300 LIKEs in one row",
            format!("SELECT count(*) FROM Big WHERE {}", rep(&like, 300, " OR ")),
        ),
        // Round 4: arithmetic on long digit text.
        (
            "(d+d+...) x 50, 400 columns",
            format!("{digits} SELECT {} FROM s", rep(&sum(50), 400, ",")),
        ),
        (
            "(d+d+...) x 50, 40 columns, 10 rows",
            format!("{ten_digits} SELECT {} FROM s", rep(&sum(50), 40, ",")),
        ),
        (
            "CAST(d AS REAL) x 20, 100 columns",
            format!(
                "{digits} SELECT {} FROM s",
                rep(
                    &format!("({})", rep("CAST(d AS REAL)", 20, " IS ")),
                    100,
                    ","
                )
            ),
        ),
        // Round 4: sorts SQLite cannot interrupt.
        (
            "sort 3,000,000 small rows",
            format!(
                "{} SELECT i FROM r ORDER BY (i*7919 % 3000000) || 'xxxxxxxxxxxxxxxx' COLLATE NOCASE",
                counter(3_000_000)
            ),
        ),
        (
            "sort 2,000 rows x 100 KiB NOCASE",
            format!(
                "{} SELECT i FROM r ORDER BY printf('%.*c', 102400, 'a') || (i*7919 % 2000) COLLATE NOCASE",
                counter(2000)
            ),
        ),
        (
            "group 3,000,000 rows into 1,000,000",
            format!(
                "{} SELECT count(*) FROM (SELECT i % 1000000 AS g, count(*) FROM r GROUP BY g)",
                counter(3_000_000)
            ),
        ),
        // Round 4: compiling nested subqueries.
        ("compile 5 levels x 30", nest(5, 30)),
        ("compile WITH 5 levels x 30", nest_cte(5, 30)),
    ]
}

fn probe_tables() -> Vec<TableInput> {
    let big = "a".repeat(262_135);
    let half = "a".repeat(131_000);
    let doc = format!(
        "{{{}}}",
        (0..21_840)
            .map(|i| format!("\"k{i:05}\":0"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let u = format!("{half}x");
    let v = format!("{half}y");
    vec![
        rows("Many", &[("t", &big)], 10),
        rows("Docs", &[("j", &doc), ("x", "$.zz")], 10),
        rows("Big", &[("t", &big)], 1),
        rows("Pair", &[("u", &u), ("v", &v)], 1),
        rows("Doc", &[("j", &doc), ("x", "$.zz")], 1),
    ]
}

fn slow_queries_stop_at_the_time_limit() -> Result<(), String> {
    let limits = limits();
    let q = QueryEngine::start(&probe_tables(), limits.clone(), &worker())
        .map_err(|e| e.to_string())?;
    let cap = limits.memory_budget / MIB + 64;
    let mut failures = Vec::new();
    for (name, sql) in probes() {
        let watch = Watch::new(q.worker_id());
        let started = Instant::now();
        let result = q.query(&sql, Page::default());
        let took = started.elapsed();
        let peak = watch.peak();
        let outcome = match &result {
            Ok(r) => format!("ran, {} rows", r.row_count),
            Err(e) => format!("{} ({})", e.code, e.what),
        };
        eprintln!(
            "  {name:42} {:>7.1} ms  worker peak {peak:>4} MiB  {outcome}",
            took.as_secs_f64() * 1000.0
        );
        if let Err(e) = &result
            && !matches!(e.code, QueryErrorCode::TimeLimit | QueryErrorCode::TooLarge)
        {
            failures.push(format!("{name}: unexpected {e}"));
        }
        if took > limits.max_time + SLACK {
            failures.push(format!("{name}: took {took:?}"));
        }
        if peak > cap {
            failures.push(format!("{name}: worker peak {peak} MiB, cap {cap} MiB"));
        }
        // The next query works, in a new worker if this one was killed.
        match q.query("SELECT length(t) FROM Big", Page::default()) {
            Ok(r) if r.rows[0][0] == 262_135 => {}
            other => failures.push(format!("{name}: next query gave {other:?}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn a_killed_worker_comes_back() -> Result<(), String> {
    let table = TableInput::new("T", vec![vec!["a".into()], vec![2.0.into()]]);
    let q =
        QueryEngine::start(&[table], Limits::default(), &worker()).map_err(|e| e.to_string())?;
    let first = q.worker_id().ok_or("no worker")?;
    // The worker capped its address space after loading.
    let caps = std::fs::read_to_string(format!("/proc/{first}/limits")).unwrap_or_default();
    let address_space = caps
        .lines()
        .find(|l| l.starts_with("Max address space"))
        .unwrap_or("");
    // "Max address space  <soft>  <hard>  bytes": the worker sets the soft one.
    let soft = address_space
        .split_whitespace()
        .nth(3)
        .unwrap_or("unlimited");
    if soft == "unlimited" {
        return Err(format!("no memory cap: {address_space}"));
    }
    eprintln!(
        "  worker {}",
        address_space
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    );
    let killed = std::process::Command::new("kill")
        .args(["-9", &first.to_string()])
        .status()
        .map_err(|e| e.to_string())?;
    if !killed.success() {
        return Err("kill failed".into());
    }
    std::thread::sleep(Duration::from_millis(50));
    let r = q
        .query("SELECT a * 21 FROM T", Page::default())
        .map_err(|e| e.to_string())?;
    if r.rows[0][0] != 42 {
        return Err(format!("got {:?}", r.rows));
    }
    let second = q.worker_id().ok_or("no worker after the kill")?;
    if second == first {
        return Err("the same worker".into());
    }
    if zombies_of_this_process() > 0 {
        return Err("the killed worker was not reaped".into());
    }
    Ok(())
}

/// The usual queries: the usability set of review round 4.
fn usual_queries() -> (Vec<TableInput>, Vec<(&'static str, String)>) {
    let regions = ["East", "West", "North", "South", "Central"];
    let mut header: Vec<String> = [
        "id", "region", "product", "qty", "price", "day", "customer", "note",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    header.extend((1..=24).map(|i| format!("c{i}")));
    let mut data = vec![header.into_iter().map(QueryCell::from).collect::<Vec<_>>()];
    for i in 0..20_000usize {
        let mut r = vec![
            QueryCell::from(i as f64),
            QueryCell::from(regions[i % 5]),
            QueryCell::from(format!("P{}", i % 97)),
            QueryCell::from((i % 13) as f64),
            QueryCell::from(((i * 37) % 1000) as f64 / 10.0),
            QueryCell::from(format!("2026-{:02}-{:02}", 1 + i % 12, 1 + i % 28)),
            QueryCell::from(format!("Cust {}", i % 500)),
            QueryCell::from(format!("note {i} lorem ipsum")),
        ];
        r.extend((1..=24).map(|c| QueryCell::from(((i * c) % 50) as f64)));
        data.push(r);
    }
    let small = |name: &str| {
        let mut rows = vec![vec!["id".into(), "k".into(), "label".into()]];
        rows.extend((0..2000).map(|i| {
            vec![
                QueryCell::from(i as f64),
                QueryCell::from((i % 97) as f64),
                QueryCell::from(format!("{name} {i}")),
            ]
        }));
        TableInput::new(name, rows)
    };
    let tables = vec![
        TableInput::new("Sales", data),
        small("A"),
        small("B"),
        small("C"),
        small("D"),
        small("E"),
    ];
    let months = (1..=12).collect::<Vec<usize>>();
    let pivot = |measures: &[String]| {
        let mut cols = vec!["region".to_owned()];
        for m in measures {
            for i in &months {
                cols.push(format!(
                    "sum(CASE WHEN substr(day,6,2) = '{i:02}' THEN {m} ELSE 0 END)"
                ));
            }
        }
        format!(
            "SELECT {} FROM Sales GROUP BY region ORDER BY region",
            cols.join(", ")
        )
    };
    let cs = |n: usize| (1..=n).map(|i| format!("c{i}")).collect::<Vec<_>>();
    let pairs = |n: usize| {
        (1..=n)
            .map(|i| format!("'c{i}', c{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let in_strs = (0..500)
        .map(|i| format!("'Cust {i}'"))
        .collect::<Vec<_>>()
        .join(",");
    let in_nums = (0..2000)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let queries = vec![
        ("json_object of 34 arguments", format!("SELECT json_object({}) FROM Sales LIMIT 5", pairs(17))),
        ("printf of 33 arguments", format!("SELECT printf('{}', {}) FROM Sales LIMIT 3", "%s,".repeat(32), cs(24).into_iter().chain(cs(8)).collect::<Vec<_>>().join(", "))),
        ("coalesce of 40 columns", format!("SELECT coalesce({}) FROM Sales LIMIT 3", cs(24).into_iter().chain(cs(16)).collect::<Vec<_>>().join(", "))),
        ("IN list of 2,000 numbers", format!("SELECT count(*) FROM Sales WHERE id IN ({in_nums})")),
        ("IN list of 500 strings NOCASE", format!("SELECT count(*) FROM Sales WHERE customer COLLATE NOCASE IN ({in_strs})")),
        ("GROUP BY 20 columns", format!("SELECT {0}, count(*) FROM Sales GROUP BY {0} ORDER BY count(*) DESC", cs(20).join(", "))),
        ("join of 6 sheets", "SELECT count(*) FROM A JOIN B ON B.k = A.k JOIN C ON C.id = B.id JOIN D ON D.id = C.id JOIN E ON E.id = D.id".into()),
        ("window functions", "SELECT id, row_number() OVER w, lag(price) OVER w, sum(qty) OVER (PARTITION BY region ORDER BY day ROWS BETWEEN 6 PRECEDING AND CURRENT ROW) FROM Sales WINDOW w AS (PARTITION BY region ORDER BY id) ORDER BY id".into()),
        ("pivot 12 months x 3 measures", pivot(&["qty".into(), "price".into(), "price*qty".into()])),
        ("pivot 12 months x 24 measures", pivot(&cs(24))),
        ("20 LIKE filters", format!("SELECT count(*) FROM Sales WHERE {}", (0..20).map(|i| format!("note LIKE '%{i}%'")).collect::<Vec<_>>().join(" OR "))),
        ("recursive calendar", "WITH RECURSIVE d(x) AS (SELECT date('2026-01-01') UNION ALL SELECT date(x, '+1 day') FROM d WHERE x < '2026-12-31') SELECT d.x, count(s.id) FROM d LEFT JOIN Sales s ON s.day = d.x GROUP BY d.x".into()),
        ("SELECT * page of 2,000 rows", "SELECT * FROM Sales ORDER BY price DESC LIMIT 2000".into()),
        ("max of 24 columns", format!("SELECT max({}) FROM Sales LIMIT 3", cs(24).join(", "))),
        ("json_extract 10 paths x 10", format!("WITH j AS (SELECT json_object({}) AS doc FROM Sales LIMIT 100) SELECT {} FROM j", pairs(10), (0..10).map(|_| format!("json_extract(doc, {})", (1..=10).map(|i| format!("'$.c{i}'")).collect::<Vec<_>>().join(", "))).collect::<Vec<_>>().join(", "))),
        ("ORDER BY NOCASE text", "SELECT customer FROM Sales ORDER BY customer COLLATE NOCASE, note COLLATE NOCASE".into()),
        ("a small lookup", "SELECT region, sum(qty) FROM Sales WHERE id < 100 GROUP BY region".into()),
    ];
    (tables, queries)
}

fn usual_queries_run_as_fast() -> Result<(), String> {
    let (tables, queries) = usual_queries();
    // Both engines live at once and share the 512 MiB budget, each with
    // its own query memory.
    let limits = Limits {
        max_memory: 128 * MIB,
        ..Limits::default()
    };
    let local = QueryEngine::load(&tables, limits.clone()).map_err(|e| e.to_string())?;
    let remote = QueryEngine::start(&tables, limits, &worker()).map_err(|e| e.to_string())?;
    let page = Page {
        offset: 0,
        max_rows: 2000,
    };
    let mut failures = Vec::new();
    let (mut in_process, mut in_worker) = (Duration::ZERO, Duration::ZERO);
    for (name, sql) in &queries {
        // Best of three each, so a busy moment on the machine does not count.
        let mut best = [Duration::MAX; 2];
        for _ in 0..3 {
            for (i, q) in [&local, &remote].into_iter().enumerate() {
                let started = Instant::now();
                let result = q.query(sql, page);
                best[i] = best[i].min(started.elapsed());
                match result {
                    Ok(_) => {}
                    Err(e) => failures.push(format!("{name}: {e}")),
                }
            }
        }
        eprintln!(
            "  {name:32} in process {:>9.1?}  in worker {:>9.1?}",
            best[0], best[1]
        );
        in_process += best[0];
        in_worker += best[1];
    }
    eprintln!(
        "  {:32} in process {in_process:>9.1?}  in worker {in_worker:>9.1?}",
        "all"
    );
    // About 20% slower at most; the margin covers a noisy machine.
    if in_worker > in_process.mul_f64(1.2) + Duration::from_millis(50) {
        failures.push(format!(
            "the worker took {in_worker:?} in all, in process {in_process:?}"
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

/// Children of this process that exited and were never waited for.
fn zombies_of_this_process() -> usize {
    let me = std::process::id().to_string();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return 0;
    };
    dir.filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
        .filter(|stat| {
            // pid (comm) state ppid ...: comm may hold spaces, so read
            // after its closing bracket.
            let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
                return false;
            };
            let mut fields = rest.split_whitespace();
            let state = fields.next();
            let ppid = fields.next();
            state == Some("Z") && ppid == Some(me.as_str())
        })
        .count()
}

/// Live children of this process.
fn children_of_this_process() -> Vec<String> {
    let me = std::process::id().to_string();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
        .filter(|stat| {
            stat.rsplit_once(')')
                .map(|(_, r)| r.split_whitespace().nth(1) == Some(me.as_str()))
                .unwrap_or(false)
        })
        .collect()
}

fn no_process_is_left_behind() -> Result<(), String> {
    // Engines dropped with a worker idle, killed, and mid-restart.
    for _ in 0..3 {
        let table = TableInput::new("T", vec![vec!["a".into()], vec![1.0.into()]]);
        let q = QueryEngine::start(&[table], limits(), &worker()).map_err(|e| e.to_string())?;
        let _ = q.query(
            "WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM r WHERE i < 3000000) SELECT i FROM r ORDER BY (i*7919 % 3000000) || 'x' COLLATE NOCASE",
            Page::default(),
        );
        let _ = q.query("SELECT a FROM T", Page::default());
    }
    let left = children_of_this_process();
    if left.is_empty() && zombies_of_this_process() == 0 {
        Ok(())
    } else {
        Err(format!("left behind: {left:?}"))
    }
}

const FAKE_ARG: &str = "__fake-worker";
const PARENT_ARG: &str = "__parent";

fn fake(args: &[&str]) -> Worker {
    let mut all = vec![FAKE_ARG];
    all.extend_from_slice(args);
    Worker::current_exe(all).expect("the test binary")
}

fn one_cell() -> Vec<TableInput> {
    vec![TableInput::new(
        "T",
        vec![vec!["a".into()], vec![2.0.into()]],
    )]
}

/// A folder of its own under the system's temporary folder, removed when
/// dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "carrysheet-worker-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch folder");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn kill(pid: u32) -> Result<(), String> {
    let status = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill {pid} failed"))
    }
}

fn read_frame(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    input.read_exact(&mut body).ok()?;
    Some(body)
}

fn write_frame(output: &mut impl Write, body: &[u8]) {
    let _ = output.write_all(&(body.len() as u32).to_le_bytes());
    let _ = output.write_all(body);
    let _ = output.flush();
}

/// A load reply with no tables: tag 1 and JSON.
const LOADED: &[u8] = b"\x01{\"tables\":[],\"bytes\":0}";

/// The fake workers:
/// - `reply <kind>`: loads nothing, then answers every query with a bad
///   frame: `json` (not JSON), `tag` (an unknown tag), `deep` (JSON nested
///   100,000 deep), `huge` (a frame header of almost 4 GiB).
/// - `slow-reload <folder> <ms>`: a real worker, but every start after the
///   first waits `ms` before it reads the load.
/// - `no-read`: never reads stdin.
/// - `say-and-die`: reads the load, writes a line naming the home folder
///   to stderr, and exits with 3.
#[expect(
    clippy::disallowed_methods,
    reason = "a fake worker writes frames to stdout"
)]
fn fake_worker(args: &[String]) -> ExitCode {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    match args.first().map(String::as_str) {
        Some("reply") => {
            if read_frame(&mut stdin).is_none() {
                return ExitCode::FAILURE;
            }
            write_frame(&mut stdout, LOADED);
            while read_frame(&mut stdin).is_some() {
                match args.get(1).map(String::as_str) {
                    Some("json") => write_frame(&mut stdout, b"\x02{not json"),
                    Some("tag") => write_frame(&mut stdout, b"\x09{}"),
                    Some("deep") => {
                        let mut body = vec![2u8];
                        body.extend(std::iter::repeat_n(b'[', 100_000));
                        body.extend(std::iter::repeat_n(b']', 100_000));
                        write_frame(&mut stdout, &body);
                    }
                    Some("huge") => {
                        let _ = stdout.write_all(&0xFFFF_FFF0u32.to_le_bytes());
                        let _ = stdout.flush();
                    }
                    _ => return ExitCode::FAILURE,
                }
            }
            ExitCode::SUCCESS
        }
        Some("slow-reload") => {
            let (Some(dir), Some(ms)) = (args.get(1), args.get(2).and_then(|m| m.parse().ok()))
            else {
                return ExitCode::FAILURE;
            };
            let count = PathBuf::from(dir).join("starts");
            let starts = std::fs::read_to_string(&count)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0)
                + 1;
            let _ = std::fs::write(&count, starts.to_string());
            if starts > 1 {
                std::thread::sleep(Duration::from_millis(ms));
            }
            drop((stdin, stdout));
            serve_worker()
        }
        Some("no-read") => {
            std::thread::sleep(Duration::from_secs(60));
            ExitCode::SUCCESS
        }
        Some("say-and-die") => {
            let _ = read_frame(&mut stdin);
            let home = std::env::var("HOME").unwrap_or_default();
            eprintln!("boom in {home}/secret.xlsx");
            ExitCode::from(3)
        }
        _ => ExitCode::FAILURE,
    }
}

/// Starts an engine, prints its worker's process id, and keeps the worker
/// busy in a 30 s query until it is killed: an idle worker would exit by
/// itself when its stdin closes, so only a busy one shows the signal works.
#[expect(clippy::disallowed_methods, reason = "prints the pid for the test")]
fn fake_parent() -> ExitCode {
    let limits = Limits {
        max_time: Duration::from_secs(30),
        max_steps: u64::MAX,
        ..Limits::default()
    };
    let Ok(q) = QueryEngine::start(&one_cell(), limits, &worker()) else {
        return ExitCode::FAILURE;
    };
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", q.worker_id().unwrap_or(0));
    let _ = stdout.flush();
    let _ = q.query(
        "WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM r) SELECT count(*) FROM r",
        Page::default(),
    );
    ExitCode::SUCCESS
}

/// Review round 5, M1: after a kill, the reload has its own time limit and
/// the query's time starts after it.
fn a_slow_reload_has_its_own_time() -> Result<(), String> {
    let scratch = Scratch::new("slow-reload");
    let dir = scratch.0.to_string_lossy().into_owned();
    let limits = Limits {
        max_time: Duration::from_millis(700),
        max_load_time: Duration::from_secs(5),
        ..Limits::default()
    };
    let w = fake(&["slow-reload", &dir, "900"]);
    let q = QueryEngine::start(&one_cell(), limits.clone(), &w).map_err(|e| e.to_string())?;
    kill(q.worker_id().ok_or("no worker")?)?;
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let r = q
        .query("SELECT a * 21 FROM T", Page::default())
        .map_err(|e| format!("after a 900 ms reload: {e}"))?;
    let took = started.elapsed();
    eprintln!("  query after a 900 ms reload: {took:.1?}");
    if r.rows[0][0] != 42 || took < Duration::from_millis(900) {
        return Err(format!("got {:?} in {took:?}", r.rows));
    }
    // A reload slower than max_load_time is a load failure, not the query's.
    let short = Limits {
        max_load_time: Duration::from_millis(300),
        ..limits
    };
    std::fs::remove_file(scratch.0.join("starts")).map_err(|e| e.to_string())?;
    drop(q);
    let q = QueryEngine::start(&one_cell(), short, &w).map_err(|e| e.to_string())?;
    kill(q.worker_id().ok_or("no worker")?)?;
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let e = q
        .query("SELECT a FROM T", Page::default())
        .err()
        .ok_or("a slow reload succeeded")?;
    let took = started.elapsed();
    eprintln!("  slow reload: {took:.1?} {}: {}", e.code, e.what);
    if e.code != QueryErrorCode::TimeLimit || !e.what.contains("loading the sheets again") {
        return Err(format!("got {e}"));
    }
    if took > Duration::from_millis(300) + SLACK {
        return Err(format!("the slow reload took {took:?}"));
    }
    Ok(())
}

/// Review round 5, M2 and L2: a reply the parent cannot read is an
/// internal error, not "needed more memory", and the worker is replaced.
fn bad_replies_are_internal_errors() -> Result<(), String> {
    for kind in ["json", "tag", "deep", "huge"] {
        let q = QueryEngine::start(&[], Limits::default(), &fake(&["reply", kind]))
            .map_err(|e| format!("{kind}: {e}"))?;
        for round in 0..2 {
            let started = Instant::now();
            let e = q
                .query("SELECT 1", Page::default())
                .err()
                .ok_or(format!("{kind}: a bad reply was taken"))?;
            let took = started.elapsed();
            if e.code != QueryErrorCode::Internal
                || e.what != "the query worker sent a reply that couldn't be read"
            {
                return Err(format!("{kind}, round {round}: {e}"));
            }
            if took > Duration::from_secs(1) {
                return Err(format!("{kind}: took {took:?}"));
            }
            if q.worker_id().is_some() {
                return Err(format!("{kind}: the worker was not stopped"));
            }
        }
        eprintln!("  {kind}: internal error, worker replaced");
    }
    if zombies_of_this_process() > 0 {
        return Err("a fake worker was not reaped".into());
    }
    Ok(())
}

/// Review round 5, M2: a worker killed from outside in the middle of a
/// query, with little memory in use, is an internal error.
fn an_outside_kill_is_not_memory() -> Result<(), String> {
    let limits = Limits {
        max_time: Duration::from_secs(5),
        max_steps: u64::MAX,
        ..Limits::default()
    };
    let q = QueryEngine::start(&one_cell(), limits, &worker()).map_err(|e| e.to_string())?;
    let pid = q.worker_id().ok_or("no worker")?;
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        kill(pid)
    });
    let e = q
        .query(
            "WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM r) SELECT count(*) FROM r",
            Page::default(),
        )
        .err()
        .ok_or("an endless query finished")?;
    killer.join().map_err(|_| "the killer panicked")??;
    eprintln!("  {}: {}", e.code, e.what);
    if e.code != QueryErrorCode::Internal || !e.what.contains("SIGKILL") {
        return Err(format!("got {e}"));
    }
    let r = q
        .query("SELECT a FROM T", Page::default())
        .map_err(|e| format!("the next query: {e}"))?;
    if r.rows[0][0] != 2 {
        return Err(format!("the next query gave {:?}", r.rows));
    }
    Ok(())
}

/// Review round 5, M4: a worker that never reads its load is killed when
/// `max_load_time` runs out.
fn a_worker_that_never_reads_is_killed() -> Result<(), String> {
    // 2 MB, far more than a pipe holds, so the write blocks.
    let big = "x".repeat(200_000);
    let mut data = vec![vec![QueryCell::from("t")]];
    data.extend((0..10).map(|_| vec![QueryCell::from(big.as_str())]));
    let limits = Limits {
        max_load_time: Duration::from_millis(500),
        ..Limits::default()
    };
    let started = Instant::now();
    let result = QueryEngine::start(&[TableInput::new("T", data)], limits, &fake(&["no-read"]));
    let took = started.elapsed();
    let e = result.err().ok_or("a worker that never reads loaded")?;
    eprintln!("  {took:.1?} {}: {}", e.code, e.what);
    if e.code != QueryErrorCode::TimeLimit {
        return Err(format!("got {e}"));
    }
    if took > Duration::from_millis(500) + SLACK {
        return Err(format!("took {took:?}"));
    }
    if !children_of_this_process().is_empty() {
        return Err("the worker is still there".into());
    }
    Ok(())
}

/// Review round 5, M3: worker engines are charged to the process's memory
/// budget (512 MiB here) with their own query memory.
fn worker_engines_share_the_budget() -> Result<(), String> {
    let limits = Limits {
        max_memory: 300 * MIB,
        ..Limits::default()
    };
    let first = QueryEngine::start(&one_cell(), limits.clone(), &worker())
        .map_err(|e| format!("the first engine: {e}"))?;
    let second = QueryEngine::start(&one_cell(), limits.clone(), &worker());
    let Err(e) = second else {
        return Err("two engines of 300 MiB fit a 512 MiB budget".into());
    };
    eprintln!("  second engine: {}: {}", e.code, e.what);
    if e.code != QueryErrorCode::TooLarge {
        return Err(format!("got {e}"));
    }
    if children_of_this_process().len() != 1 {
        return Err("the refused engine started a worker".into());
    }
    drop(first);
    QueryEngine::start(&one_cell(), limits, &worker())
        .map_err(|e| format!("after the first was dropped: {e}"))?;
    Ok(())
}

/// Review round 5, L1: on Linux a worker dies with its parent.
fn the_worker_dies_with_its_parent() -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut parent = Command::new(exe)
        .arg(PARENT_ARG)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    let mut stdout = parent.stdout.take().ok_or("no stdout")?;
    let mut byte = [0u8; 1];
    while stdout.read(&mut byte).map_err(|e| e.to_string())? == 1 && byte[0] != b'\n' {
        line.push(byte[0] as char);
    }
    let pid: u32 = line
        .trim()
        .parse()
        .map_err(|_| format!("bad pid {line:?}"))?;
    if pid == 0 {
        return Err("the parent has no worker".into());
    }
    // Let the worker get into the query.
    std::thread::sleep(Duration::from_millis(200));
    let _ = parent.kill();
    let _ = parent.wait();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| {
                s.rsplit_once(')')
                    .and_then(|(_, r)| r.trim().chars().next())
            });
        if matches!(state, None | Some('Z')) {
            eprintln!("  worker gone {:.1?} after its parent", started.elapsed());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = kill(pid);
    Err("the worker outlived its parent by 2 s".into())
}

/// Review round 5, L5: a worker's stderr is kept, not shown, and its start
/// is in internal errors without the home folder.
fn stderr_is_kept_and_scrubbed() -> Result<(), String> {
    let e = QueryEngine::start(&one_cell(), Limits::default(), &fake(&["say-and-die"]))
        .err()
        .ok_or("a dead worker loaded")?;
    eprintln!("  {}: {} / {}", e.code, e.what, e.why);
    if e.code != QueryErrorCode::Internal
        || !e.why.contains("The worker said: boom in ~/secret.xlsx")
    {
        return Err(format!("got {e}"));
    }
    if let Ok(home) = std::env::var("HOME")
        && home.len() > 1
        && e.to_string().contains(&home)
    {
        return Err("the home folder is in the error".into());
    }
    Ok(())
}

/// Review round 5, L2: the parent reads a query reply of at most about
/// 55 MiB with the default limits, and a page that fills its limits fits.
fn a_query_frame_has_a_cap() -> Result<(), String> {
    let limits = Limits {
        max_value_bytes: 256 * 1024,
        ..Limits::default()
    };
    // Control characters, which JSON writes six times longer.
    let cell = "\u{1}".repeat(250_000);
    let mut data = vec![vec![QueryCell::from("t")]];
    data.extend((0..40).map(|_| vec![QueryCell::from(cell.as_str())]));
    let q = QueryEngine::start(&[TableInput::new("T", data)], limits, &worker())
        .map_err(|e| e.to_string())?;
    let r = q
        .query("SELECT t FROM T", Page::default())
        .map_err(|e| format!("a full page: {e}"))?;
    eprintln!("  a page of {} rows of 250,000 escaped bytes", r.row_count);
    if r.row_count < 30 {
        return Err(format!("only {} rows", r.row_count));
    }
    Ok(())
}
