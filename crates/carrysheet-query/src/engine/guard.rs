//! What keeps a query read-only and bounded: database settings, SQLite
//! limits, the authorizer and the progress handler.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use rusqlite::config::DbConfig;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::{Connection, Result};

use crate::input::shown_word;
use crate::output::Limits;

/// How many virtual-machine steps pass between progress-handler calls.
/// SQLite calls the handler only when its program jumps (to the next row,
/// say), so a check may come later than this, never sooner. At 100, a scan
/// with one slow call per row ran 20 calls past its time limit; at 10 it is
/// a few. What runs between two jumps is bounded by [`super::cost`]. The
/// handler only counts and reads a flag the [`Watchdog`] sets, so checking
/// this often costs little (about 5%).
pub(crate) const STEPS_PER_CHECK: u64 = 10;

/// Why the progress handler stopped a query.
pub(crate) const STOPPED_NONE: u8 = 0;
pub(crate) const STOPPED_STEPS: u8 = 1;
pub(crate) const STOPPED_TIME: u8 = 2;

/// Settings applied right after opening, before loading: no triggers, no
/// double-quoted strings (so a misspelled `"column"` is an error, not text),
/// defensive mode (no writable schema, no shadow-table writes).
pub(crate) fn harden(conn: &Connection) -> Result<()> {
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_WRITABLE_SCHEMA, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE, false)?;
    // Sorts and temporary results stay in memory, under the heap limit
    // (budget.rs); on disk they could fill it (a 2.6 GB temp file was measured for
    // one runaway ORDER BY within the step limit).
    conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
    Ok(())
}

/// Something the authorizer refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Denied {
    /// A statement or part of one that is not a read, such as `ATTACH`.
    Statement(String),
    /// A function not on [`ALLOWED_FUNCTIONS`].
    Function(String),
    /// A table that is not a loaded sheet or one of [`ALLOWED_READS`].
    Read(String),
}

/// What the authorizer saw while the last statement was prepared.
#[derive(Debug, Default)]
pub(crate) struct Seen {
    /// The first thing it refused.
    pub denied: Option<Denied>,
    /// Columns read, as (table, column), at most [`MAX_READS`].
    pub reads: Vec<(String, String)>,
}

/// Columns remembered per statement, for notes about error cells.
const MAX_READS: usize = 1000;

/// Longest LIKE or GLOB pattern, in bytes. Our matcher stops at the time
/// limit, so this only keeps its recursion shallow (one level per wildcard).
pub(crate) const MAX_LIKE_PATTERN: usize = 100;

/// The functions a query may call; every other one is refused. Each is fast
/// on any value up to `Limits::max_value_bytes` (linear or better), or is
/// bounded by that limit. Left out on purpose, among others: `unhex` (the
/// two-argument form is quadratic), `json_patch`, `json_set`, `json_insert`,
/// `json_replace`, `json_remove` (slow on large documents), `soundex`,
/// `changes`, `sqlite_compileoption_*`. Table-valued `json_each` is allowed
/// as a table ([`ALLOWED_READS`]); `json_tree` is not.
pub const ALLOWED_FUNCTIONS: &[&str] = &[
    // Core scalar functions. trim, ltrim, rtrim (with a character set),
    // instr and replace are Carrysheet's linear versions (functions.rs).
    "abs",
    "char",
    "coalesce",
    "concat",
    "concat_ws",
    "format",
    "glob",
    "hex",
    "if",
    "ifnull",
    "iif",
    "instr",
    "length",
    "like",
    "likelihood",
    "likely",
    "lower",
    "ltrim",
    "max",
    "min",
    "nullif",
    "octet_length",
    "printf",
    "quote",
    "random",
    "randomblob",
    "replace",
    "round",
    "rtrim",
    "sign",
    "sqlite_version",
    "substr",
    "substring",
    "trim",
    "typeof",
    "unicode",
    "unlikely",
    "upper",
    "zeroblob",
    // Dates and times.
    "current_date",
    "current_time",
    "current_timestamp",
    "date",
    "datetime",
    "julianday",
    "strftime",
    "time",
    "timediff",
    "unixepoch",
    // Aggregates (min and max above), bounded by max_value_bytes.
    "avg",
    "count",
    "group_concat",
    "string_agg",
    "sum",
    "total",
    // Window functions.
    "cume_dist",
    "dense_rank",
    "first_value",
    "lag",
    "last_value",
    "lead",
    "nth_value",
    "ntile",
    "percent_rank",
    "rank",
    "row_number",
    // JSON: reading and building, on documents up to max_value_bytes.
    "->",
    "->>",
    "json_array",
    "json_array_length",
    "json_extract",
    "json_object",
    "json_type",
    "json_valid",
];

/// Tables a query may read besides the loaded sheets.
pub const ALLOWED_READS: &[&str] = &[
    "sqlite_schema",
    "sqlite_master",
    "sqlite_temp_schema",
    "sqlite_temp_master",
    "json_each",
    "pragma_table_info",
    "pragma_table_xinfo",
    "pragma_table_list",
];

/// Locks the loaded database: SQLite limits, linear string functions,
/// `query_only`, then the authorizer, which records what it saw in `seen`.
pub(crate) fn lock(
    conn: &Connection,
    limits: &Limits,
    tables: &[String],
    seen: Arc<Mutex<Seen>>,
    late: &Arc<AtomicBool>,
) -> Result<()> {
    let mut readable: HashSet<String> = tables.iter().map(|t| t.to_ascii_lowercase()).collect();
    readable.extend(ALLOWED_READS.iter().map(|t| (*t).to_owned()));
    let clamp = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, clamp(limits.max_value_bytes))?;
    // Our own check gives the clearer error; this one is the backstop.
    conn.set_limit(
        Limit::SQLITE_LIMIT_SQL_LENGTH,
        clamp(limits.max_sql_bytes.saturating_add(1)),
    )?;
    conn.set_limit(Limit::SQLITE_LIMIT_COLUMN, 2000)?;
    conn.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 500)?;
    conn.set_limit(Limit::SQLITE_LIMIT_COMPOUND_SELECT, 100)?;
    conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    conn.set_limit(Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 1)?;
    conn.set_limit(Limit::SQLITE_LIMIT_WORKER_THREADS, 0)?;
    conn.set_limit(
        Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
        clamp(MAX_LIKE_PATTERN),
    )?;
    // A bound on the compiled program; see cost.rs.
    conn.set_limit(
        Limit::SQLITE_LIMIT_VDBE_OP,
        clamp(super::cost::MAX_INSTRUCTIONS),
    )?;
    super::functions::register(conn, limits.max_value_bytes, MAX_LIKE_PATTERN, late)?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    conn.authorizer(Some(move |ctx: AuthContext<'_>| {
        let verdict = authorize(&ctx.action, &readable);
        if let Ok(mut seen) = seen.lock() {
            match (&verdict, &ctx.action) {
                (Err(what), _) if seen.denied.is_none() => seen.denied = Some(what.clone()),
                (
                    Ok(()),
                    AuthAction::Read {
                        table_name,
                        column_name,
                    },
                ) if seen.reads.len() < MAX_READS => {
                    seen.reads
                        .push(((*table_name).to_owned(), (*column_name).to_owned()));
                }
                _ => {}
            }
        }
        match verdict {
            Ok(()) => Authorization::Allow,
            Err(_) => Authorization::Deny,
        }
    }))?;
    Ok(())
}

/// Pragmas that only read the schema. Everything else is refused.
const READ_PRAGMAS: [&str; 3] = ["table_info", "table_xinfo", "table_list"];

/// Functions that the workspace build leaves out of SQLite altogether
/// (`.cargo/config.toml`). A "no such function" error for them is reported
/// as refused, the same as in builds that have them.
pub(crate) const DENIED_FUNCTIONS: [&str; 2] = ["load_extension", "fts3_tokenizer"];

/// True for SQLite's own tables, such as `sqlite_schema`.
fn is_schema_table(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("sqlite_")
}

/// Allows reading only: the loaded tables, vetted functions. `Err` says what
/// was refused, for the error message.
fn authorize(
    action: &AuthAction<'_>,
    readable: &HashSet<String>,
) -> std::result::Result<(), Denied> {
    statement(action)
        .map_err(Denied::Statement)
        .and_then(|()| match action {
            AuthAction::Function { function_name }
                if !ALLOWED_FUNCTIONS
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(function_name)) =>
            {
                Err(Denied::Function(function_name.to_ascii_lowercase()))
            }
            AuthAction::Read {
                table_name,
                column_name,
            } if !may_read(table_name, column_name, readable) => {
                Err(Denied::Read((*table_name).to_owned()))
            }
            _ => Ok(()),
        })
}

/// Table-valued functions SQLite may have built in, besides the
/// `pragma_*` ones.
const TABLE_FUNCTIONS: [&str; 5] = [
    "json_tree",
    "dbstat",
    "sqlite_dbpage",
    "sqlite_stmt",
    "generate_series",
];

/// Whether a query may read `table`. A column read names a real table, which
/// must be loaded or allowed. A read with no column is SQLite telling us a
/// FROM item is used without its columns (as in `count(*)`); it names the
/// item as written, which may be a WITH table, so only table-valued functions
/// are refused there.
fn may_read(table: &str, column: &str, readable: &HashSet<String>) -> bool {
    let table = table.to_ascii_lowercase();
    if readable.contains(&table) {
        return true;
    }
    column.is_empty() && !table.starts_with("pragma_") && !TABLE_FUNCTIONS.contains(&table.as_str())
}

/// The statement-level check: reads, functions and table reads pass here.
fn statement(action: &AuthAction<'_>) -> std::result::Result<(), String> {
    match action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Recursive
        | AuthAction::Function { .. } => Ok(()),
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } => {
            if READ_PRAGMAS
                .iter()
                .any(|p| p.eq_ignore_ascii_case(pragma_name))
            {
                Ok(())
            } else {
                Err(match pragma_value {
                    Some(v) => format!("PRAGMA {} = {}", shown_word(pragma_name), shown_word(v)),
                    None => format!("PRAGMA {}", shown_word(pragma_name)),
                })
            }
        }
        AuthAction::Attach { .. } => Err("ATTACH".into()),
        AuthAction::Detach { .. } => Err("DETACH".into()),
        // CREATE, ALTER and DROP first write SQLite's own schema table, and
        // that refusal stops the statement; name the statement, not the table.
        AuthAction::Insert { table_name } if is_schema_table(table_name) => Err("CREATE".into()),
        AuthAction::Update { table_name, .. } if is_schema_table(table_name) => {
            Err("ALTER TABLE".into())
        }
        AuthAction::Delete { table_name } if is_schema_table(table_name) => Err("DROP".into()),
        AuthAction::Insert { table_name } => Err(format!("INSERT INTO {}", shown_word(table_name))),
        AuthAction::Update { table_name, .. } => Err(format!("UPDATE {}", shown_word(table_name))),
        AuthAction::Delete { table_name } => Err(format!("DELETE FROM {}", shown_word(table_name))),
        AuthAction::CreateTable { .. }
        | AuthAction::CreateTempTable { .. }
        | AuthAction::CreateIndex { .. }
        | AuthAction::CreateTempIndex { .. }
        | AuthAction::CreateView { .. }
        | AuthAction::CreateTempView { .. }
        | AuthAction::CreateTrigger { .. }
        | AuthAction::CreateTempTrigger { .. }
        | AuthAction::CreateVtable { .. } => Err("CREATE".into()),
        AuthAction::DropTable { .. }
        | AuthAction::DropTempTable { .. }
        | AuthAction::DropIndex { .. }
        | AuthAction::DropTempIndex { .. }
        | AuthAction::DropView { .. }
        | AuthAction::DropTempView { .. }
        | AuthAction::DropTrigger { .. }
        | AuthAction::DropTempTrigger { .. }
        | AuthAction::DropVtable { .. } => Err("DROP".into()),
        AuthAction::AlterTable { .. } => Err("ALTER TABLE".into()),
        AuthAction::Transaction { .. } => Err("BEGIN, COMMIT and ROLLBACK".into()),
        AuthAction::Savepoint { .. } => Err("SAVEPOINT".into()),
        AuthAction::Reindex { .. } => Err("REINDEX".into()),
        AuthAction::Analyze { .. } => Err("ANALYZE".into()),
        _ => Err("this kind of statement".into()),
    }
}

/// One timer thread for every query in this process: it sets each query's
/// `late` flag once its deadline passes, so the progress handler reads a
/// flag instead of the clock.
///
/// It is started once, when the first sheets load. A thread started per
/// query broke the worker: after a large query, freed memory stays mapped,
/// so under the worker's address-space cap a new thread could not map its
/// signal stack, and Rust aborts the whole process then, which looked like
/// a query that needed too much memory. One thread started before the cap
/// maps nothing later.
struct Timer {
    alarms: Mutex<Alarms>,
    wake: Condvar,
}

struct Alarms {
    next_id: u64,
    /// Each running query's id, deadline and `late` flag.
    waiting: Vec<(u64, Instant, Arc<AtomicBool>)>,
}

static TIMER: Timer = Timer {
    alarms: Mutex::new(Alarms {
        next_id: 0,
        waiting: Vec::new(),
    }),
    wake: Condvar::new(),
};

/// Whether the timer thread started; false if it could not, and then each
/// query reads the clock instead.
static TIMER_RUNS: OnceLock<bool> = OnceLock::new();

/// Starts the timer thread if it is not running yet. Called on load, so a
/// worker starts it before it caps its memory.
pub(crate) fn start_timer() -> bool {
    *TIMER_RUNS.get_or_init(|| {
        std::thread::Builder::new()
            .name("carrysheet-query-timer".to_owned())
            .stack_size(64 * 1024)
            .spawn(|| TIMER.run())
            .is_ok()
    })
}

impl Timer {
    /// Never returns. Nothing panics while holding the lock, and a poisoned
    /// lock is taken anyway: a timer that stopped would leave queries
    /// without a time limit.
    fn run(&self) {
        let mut alarms = self.alarms.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = Instant::now();
            alarms.waiting.retain(|(_, deadline, late)| {
                if *deadline <= now {
                    late.store(true, Ordering::Relaxed);
                    false
                } else {
                    true
                }
            });
            let next = alarms
                .waiting
                .iter()
                .map(|(_, deadline, _)| *deadline)
                .min();
            alarms = match next {
                Some(deadline) => {
                    self.wake
                        .wait_timeout(alarms, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .wake
                    .wait(alarms)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

/// One query's alarm on the timer thread; removed when dropped.
pub(crate) struct Watchdog {
    alarm: Option<u64>,
}

impl Watchdog {
    /// Sets an alarm for `deadline`; without a timer thread, `alarm` is
    /// `None`.
    fn start(deadline: Instant, late: Arc<AtomicBool>) -> Watchdog {
        if !start_timer() {
            return Watchdog { alarm: None };
        }
        let mut alarms = TIMER.alarms.lock().unwrap_or_else(PoisonError::into_inner);
        let id = alarms.next_id;
        alarms.next_id += 1;
        alarms.waiting.push((id, deadline, late));
        drop(alarms);
        TIMER.wake.notify_one();
        Watchdog { alarm: Some(id) }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        if let Some(id) = self.alarm {
            let mut alarms = TIMER.alarms.lock().unwrap_or_else(PoisonError::into_inner);
            alarms.waiting.retain(|(other, _, _)| *other != id);
        }
    }
}

/// Installs a progress handler that stops the query after `limits.max_steps`
/// steps or `limits.max_time`, and records which in `stopped`.
/// `start` is when the query was asked for: waiting for another query counts
/// toward `limits.max_time`. Keep the returned [`Watchdog`] until the query
/// is done.
pub(crate) fn watch(
    conn: &Connection,
    limits: &Limits,
    start: Instant,
    stopped: Arc<AtomicU8>,
    late: &Arc<AtomicBool>,
) -> Result<Watchdog> {
    stopped.store(STOPPED_NONE, Ordering::Relaxed);
    late.store(false, Ordering::Relaxed);
    let max_checks = limits.max_steps.div_ceil(STEPS_PER_CHECK).max(1);
    let max_time = limits.max_time;
    let late = Arc::clone(late);
    let deadline = start.checked_add(max_time);
    let watchdog = match deadline {
        Some(deadline) => Watchdog::start(deadline, Arc::clone(&late)),
        None => Watchdog { alarm: None },
    };
    // Without a timer thread, read the clock: slower, same limit.
    let clock = deadline.is_some() && watchdog.alarm.is_none();
    let mut checks = 0u64;
    let period = i32::try_from(STEPS_PER_CHECK).unwrap_or(i32::MAX);
    conn.progress_handler(
        period,
        Some(move || {
            checks += 1;
            if clock && start.elapsed() > max_time {
                // Our functions read the flag too.
                late.store(true, Ordering::Relaxed);
            }
            let why = if checks > max_checks {
                STOPPED_STEPS
            } else if late.load(Ordering::Relaxed) {
                STOPPED_TIME
            } else {
                return false;
            };
            stopped.store(why, Ordering::Relaxed);
            true
        }),
    )?;
    Ok(watchdog)
}

/// Removes the progress handler.
pub(crate) fn unwatch(conn: &Connection) -> Result<()> {
    conn.progress_handler(0, None::<fn() -> bool>)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        harden(&conn).unwrap();
        conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);")
            .unwrap();
        lock(
            &conn,
            &Limits::default(),
            &["t".to_owned()],
            Arc::default(),
            &Arc::default(),
        )
        .unwrap();
        conn
    }

    #[test]
    fn the_authorizer_alone_refuses_writes_and_attach() {
        let conn = locked();
        for sql in [
            "ATTACH ':memory:' AS other",
            "DETACH main",
            "CREATE TABLE u (x)",
            "INSERT INTO t VALUES (2)",
            "PRAGMA writable_schema = ON",
            "PRAGMA query_only = OFF",
        ] {
            let err = conn.execute_batch(sql).unwrap_err().to_string();
            assert!(err.contains("not authorized"), "{sql}: {err}");
        }
        // The workspace build leaves load_extension out of SQLite altogether.
        let err = conn
            .execute_batch("SELECT load_extension('x')")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not authorized") || err.contains("no such function"),
            "{err}"
        );
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn query_only_alone_refuses_writes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (x); PRAGMA query_only = ON;")
            .unwrap();
        let err = conn.execute_batch("INSERT INTO t VALUES (1)").unwrap_err();
        assert!(err.to_string().contains("readonly"), "{err}");
    }

    fn check(action: AuthAction<'_>) -> std::result::Result<(), Denied> {
        let mut readable: HashSet<String> = ["data".to_owned()].into();
        readable.extend(ALLOWED_READS.iter().map(|t| (*t).to_owned()));
        authorize(&action, &readable)
    }

    #[test]
    fn only_reads_are_allowed() {
        assert!(check(AuthAction::Select).is_ok());
        assert!(
            check(AuthAction::Function {
                function_name: "sum"
            })
            .is_ok()
        );
        assert!(
            check(AuthAction::Function {
                function_name: "SUM"
            })
            .is_ok()
        );
        assert_eq!(
            check(AuthAction::Function {
                function_name: "unhex"
            }),
            Err(Denied::Function("unhex".to_owned()))
        );
        assert!(
            check(AuthAction::Read {
                table_name: "Data",
                column_name: "x"
            })
            .is_ok()
        );
        assert_eq!(
            check(AuthAction::Read {
                table_name: "json_tree",
                column_name: "key"
            }),
            Err(Denied::Read("json_tree".to_owned()))
        );
        // A read with no column names a FROM item as written: a WITH table
        // passes, a table-valued function does not.
        let no_column = |table_name| {
            check(AuthAction::Read {
                table_name,
                column_name: "",
            })
        };
        assert!(no_column("my_cte").is_ok());
        assert!(no_column("Data").is_ok());
        assert!(no_column("json_tree").is_err());
        assert!(no_column("PRAGMA_function_list").is_err());
        assert!(no_column("pragma_table_info").is_ok());
        assert!(
            check(AuthAction::Pragma {
                pragma_name: "table_info",
                pragma_value: Some("Data"),
            })
            .is_ok()
        );
        assert_eq!(
            check(AuthAction::Pragma {
                pragma_name: "query_only",
                pragma_value: Some("OFF"),
            }),
            Err(Denied::Statement("PRAGMA query_only = OFF".to_owned()))
        );
        assert_eq!(
            check(AuthAction::Attach { filename: "x.db" }),
            Err(Denied::Statement("ATTACH".to_owned()))
        );
    }

    /// Every function in this SQLite build is either on the list or refused,
    /// and every name on the list exists (no typos).
    #[test]
    fn the_function_list_is_vetted() {
        let conn = Connection::open_in_memory().unwrap();
        let mut stmt = conn
            .prepare("SELECT DISTINCT name FROM pragma_function_list ORDER BY name")
            .unwrap();
        let built_in: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for name in ALLOWED_FUNCTIONS {
            assert!(
                built_in.iter().any(|b| b == name),
                "{name} is not in SQLite"
            );
        }
        let refused: Vec<&str> = built_in
            .iter()
            .map(String::as_str)
            .filter(|name| !ALLOWED_FUNCTIONS.contains(name))
            .collect();
        for name in &refused {
            assert_eq!(
                check(AuthAction::Function {
                    function_name: name
                }),
                Err(Denied::Function((*name).to_owned())),
                "{name}"
            );
        }
        for name in [
            "unhex",
            "json_patch",
            "json_set",
            "json_insert",
            "json_replace",
            "json_remove",
            "jsonb_set",
            "soundex",
            "changes",
            "sqlite_compileoption_used",
        ] {
            assert!(refused.contains(&name), "{name} should be refused");
        }
    }

    /// The flags in `.cargo/config.toml` took effect: a build that ignored
    /// LIBSQLITE3_FLAGS would fail here.
    #[test]
    fn sqlite_is_built_with_the_workspace_flags() {
        let conn = Connection::open_in_memory().unwrap();
        let used = |opt: &str| -> i64 {
            conn.query_row("SELECT sqlite_compileoption_used(?1)", [opt], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(used("ENABLE_FTS5"), 0);
        assert_eq!(used("ENABLE_FTS3"), 0);
        assert_eq!(used("ENABLE_RTREE"), 0);
        assert_eq!(used("DQS=0"), 1);
        assert_eq!(used("OMIT_LOAD_EXTENSION"), 1);
        assert_eq!(used("ENABLE_DBSTAT_VTAB"), 0);
        assert_eq!(used("ENABLE_STAT4"), 0);
        assert_eq!(used("OMIT_DEPRECATED"), 1);
        assert_eq!(used("OMIT_SHARED_CACHE"), 1);
    }

    /// The one timer thread sets a query's flag at its deadline, and an
    /// alarm dropped first (the query finished) never sets it.
    #[test]
    fn the_shared_timer_sets_late_flags() {
        use std::time::Duration;
        assert!(start_timer());
        let late = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let soon = Instant::now() + Duration::from_millis(20);
        let alarm = Watchdog::start(soon, Arc::clone(&late));
        assert!(alarm.alarm.is_some());
        drop(Watchdog::start(soon, Arc::clone(&finished)));
        let waited = Instant::now();
        while !late.load(Ordering::Relaxed) && waited.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(late.load(Ordering::Relaxed), "the deadline passed unseen");
        assert!(!finished.load(Ordering::Relaxed), "a dropped alarm fired");
    }
}
