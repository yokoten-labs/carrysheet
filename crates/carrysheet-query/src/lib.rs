//! Read-only SQL over sheets and tables.
//!
//! Give [`QueryEngine::start`] a list of [`TableInput`]s (a sheet's used
//! range, or an Excel table), then run `SELECT` queries on them with
//! [`QueryEngine::query`]. Each input becomes a table in a private, in-memory
//! SQLite database in a worker process:
//!
//! - The first row gives the column names by default. An empty header becomes
//!   the column letter (`A`, `B`, ...), a repeated one gets `_2`, `_3`, ...
//!   CJK and other names are kept; every name is quoted, so a sheet called
//!   `Sales 2024` is queried as `SELECT * FROM "Sales 2024"`.
//! - Each column gets a type from its values ([`Affinity`]), but every value
//!   keeps its own type: a text cell in a number column stays text.
//!   Booleans are stored as 1 and 0 and come back as `true` and `false`.
//!   Dates are ISO 8601 text ([`QueryCell::Date`], [`excel_serial_to_iso`]).
//! - After loading, the database is read-only: writes, `ATTACH`,
//!   extensions and settings are refused with an error that says why.
//!   Every query is bounded in steps, time and memory ([`Limits`]), and
//!   results come a page at a time ([`Page`], [`QueryResult::next_offset`]).
//!
//! # How long a query can run
//!
//! A query from [`QueryEngine::start`] runs in a worker process: the same
//! program started again with an argument that makes it call
//! [`serve_worker`] ([`Worker`] says how). When no answer has come 0.2 s
//! after [`Limits::max_time`], the parent kills the worker and answers
//! `time_limit`. Killing and reaping the worker took a few milliseconds on
//! the development machine, so once it runs, a query ends at most about
//! max_time + 0.2 s later, whatever it does: a sort, a row full of
//! arithmetic on long text, compiling nested subqueries. What can come
//! before that:
//!
//! - Waiting for another query on the same engine; it counts toward
//!   max_time.
//! - After a worker was killed, the next query first loads the sheets into
//!   a new worker. That load has its own [`Limits::max_load_time`] (plus
//!   0.2 s); when it runs out the query fails as `time_limit` without
//!   running, saying it was the load. Otherwise the query's max_time starts
//!   when the new worker is ready.
//!
//! The first load is bounded the same way: a worker that has not read the
//! sheets and answered within max_load_time + 0.2 s is killed, so
//! [`QueryEngine::start`] never waits longer.
//!
//! A worker that answers something unreadable (malformed, or larger than a
//! page can be) is killed and reported as an internal error. One that dies
//! by itself is `too_large` when it aborted (an allocation failed) or was
//! killed with its memory near its share; otherwise internal. The start of
//! a worker's stderr is kept for those errors (home folder shown as `~`)
//! and not shown otherwise. On Linux a worker also dies with its parent.
//!
//! On Linux the worker caps its address space after loading (`RLIMIT_AS`:
//! what it uses then, plus [`Limits::max_memory`], plus 64 MiB); a worker
//! that goes past the cap stops, and the query fails as `too_large`. On
//! macOS and Windows only SQLite's heap limit bounds its memory. Each
//! engine from [`QueryEngine::start`] is charged to the calling process's
//! [`Limits::memory_budget`] with its data, the copy of the sheets kept to
//! restart its worker, and its own max_memory, because its queries run
//! beside other engines' queries.
//!
//! Inside the worker, SQLite stops most queries at the time limit itself,
//! with a clearer error; it checks the time between steps of its program.
//! Results cross a pipe as JSON, which on the development machine added
//! 4 to 6% to a set of usual queries (25 to 30 ms to a page of 2,000 rows of 32
//! columns).
//!
//! [`QueryEngine::load`] runs queries in the caller's process instead, for
//! tests and for builds without processes. There only SQLite's checks bound
//! time, and a single step (a sort, a long row, compiling) can run past
//! the limit by seconds.
//!
//! # What a query may call
//!
//! - Functions are allowed by list ([`ALLOWED_FUNCTIONS`]); every other
//!   one is refused with a `not_allowed` error. Each allowed function is
//!   linear or better on values up to [`Limits::max_value_bytes`] (256 KiB
//!   by default). `max`, `min` and `json_extract`, which read a whole value
//!   per argument, take at most 32 arguments.
//! - LIKE, GLOB, `instr`, `replace` and `trim`/`ltrim`/`rtrim` with a
//!   character set are Carrysheet's own versions with SQLite's results.
//!   They read the time limit every 4 KB of work and stop the query when it
//!   has passed. LIKE and GLOB patterns are at most 100 bytes.
//! - After compiling a query, Carrysheet lists the compiled program
//!   (`EXPLAIN`), which includes the copies SQLite makes of a subquery's
//!   or WITH table's expressions in every place that uses them, and refuses
//!   as `too_large` a query whose one row could not finish within the
//!   default time limit: each instruction counts the longest it took on
//!   256 KiB values (a function call, arithmetic or an instruction not
//!   known to be fast 450 µs, a NOCASE comparison 150 µs, ...), and the sum
//!   may be at most 1 s. Programs over 50,000 instructions are refused
//!   while they compile. This is an early, cheap refusal; the worker
//!   process is what bounds the time.
//!
//! Tables a query may read are the loaded ones plus [`ALLOWED_READS`]
//! (`sqlite_schema`, `json_each`, `pragma_table_info`, ...).
//!
//! Queries on one engine run one at a time, and so do queries and loads in
//! one process. Waiting for another one counts toward [`Limits::max_time`]
//! (or [`Limits::max_load_time`]).
//!
//! # How SQLite is built
//!
//! Builds inside the Carrysheet repository take SQLite's compile options
//! from `LIBSQLITE3_FLAGS` in `.cargo/config.toml`: no full-text search,
//! R-tree, dbstat or extension loading, and no double-quoted strings. A
//! build outside the repository (`cargo install`, or this crate used as a
//! dependency) does not read that file and gets libsqlite3-sys's default
//! options, so SQLite is larger and has those features compiled in. They
//! stay unusable from a query: the authorizer allows only the functions in
//! [`ALLOWED_FUNCTIONS`] and the tables in [`ALLOWED_READS`], and
//! extension loading is never turned on. Double-quoted strings are turned
//! off per connection either way.
//!
//! # Web build
//!
//! SQLite is C code. On `wasm32` targets this crate builds without it: the
//! input types, header naming and date helpers are there, but `QueryEngine`
//! is not, so the web trial page has no `query` command (see
//! `docs/web-build.md`). A browser has no child processes either; a web
//! build of the engine would run it in a Web Worker, which the page can
//! terminate the same way.

// Without the engine, some helpers are used only by tests.
#![cfg_attr(target_family = "wasm", allow(dead_code))]

mod date;
mod error;
mod input;
mod output;

#[cfg(not(target_family = "wasm"))]
mod engine;
#[cfg(not(target_family = "wasm"))]
mod process;

pub use date::{excel_serial_to_iso, excel_serial_to_iso_with_format};
#[cfg(not(target_family = "wasm"))]
pub use engine::{ALLOWED_FUNCTIONS, ALLOWED_READS};
pub use error::{QueryError, QueryErrorCode};
pub use input::{Affinity, ColumnInfo, Header, QueryCell, TableInfo, TableInput, quote_ident};
pub use output::{Limits, Page, QueryResult, ResultColumn};
#[cfg(not(target_family = "wasm"))]
pub use process::{QueryEngine, Worker, serve_worker};
