//! Where queries run: in a worker process the caller can kill
//! ([`QueryEngine::start`]), or in the caller's own process
//! ([`QueryEngine::load`]).
//!
//! SQLite checks the time limit only between steps of its program, and some
//! steps can run for seconds: arithmetic on long text, a sort, compiling a
//! query whose subqueries copy each other. A worker process bounds them all:
//! when no reply has come [`GRACE`](parent::GRACE) after the time limit,
//! the parent kills the worker, answers `time_limit`, and starts a new
//! worker with the same sheets for the next query.

mod frame;
mod parent;
mod worker;

use std::ffi::OsString;
use std::path::PathBuf;

pub use worker::serve_worker;

use crate::engine::{Local, check_size};
use crate::error::QueryError;
use crate::input::{TableInfo, TableInput};
use crate::output::{Limits, Page, QueryResult};

/// How to start a query worker: a program that calls [`serve_worker`] when
/// it gets these arguments. Usually the running program itself, with a
/// hidden subcommand:
///
/// ```no_run
/// # fn main() -> std::io::Result<()> {
/// let worker = carrysheet_query::Worker::current_exe(["__query-worker"])?;
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    program: PathBuf,
    args: Vec<OsString>,
}

impl Worker {
    /// Starts `program` with `args`.
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Worker {
        Worker {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// Starts the running program again with `args`.
    pub fn current_exe(
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> std::io::Result<Worker> {
        Ok(Worker::new(std::env::current_exe()?, args))
    }
}

/// Loaded tables in a private, read-only, in-memory SQLite database.
///
/// Load once, then call [`QueryEngine::query`] as often as needed; each
/// call returns one page of the result.
pub struct QueryEngine {
    inner: Inner,
}

enum Inner {
    Local(Local),
    Remote(parent::Remote),
}

impl QueryEngine {
    /// Loads the tables into a worker process that `worker` starts, and runs
    /// every query there. Use this in programs: a query that runs past
    /// [`Limits::max_time`] is stopped at most about 0.2 s later, whatever
    /// it does, by killing the worker. Each input becomes one table; see
    /// [`QueryEngine::tables`] for the names and types it got.
    ///
    /// Loading, sending the sheets included, is bounded by
    /// [`Limits::max_load_time`] plus 0.2 s; a worker that has not answered
    /// by then is killed. The engine keeps a copy of the request it sent, to
    /// load the sheets into a new worker after one was killed; that reload
    /// has its own [`Limits::max_load_time`], and the next query's time
    /// starts when it is done. The engine is charged to
    /// [`Limits::memory_budget`] with its data, that copy, and its own
    /// [`Limits::max_memory`].
    pub fn start(
        inputs: &[TableInput],
        limits: Limits,
        worker: &Worker,
    ) -> Result<QueryEngine, QueryError> {
        check_size(inputs, &limits)?;
        let remote = parent::Remote::start(worker.clone(), inputs, limits)?;
        Ok(QueryEngine {
            inner: Inner::Remote(remote),
        })
    }

    /// Loads the tables in this process. Queries are bounded only as far as
    /// SQLite can stop them: most stop at the time limit, but a single step
    /// (a sort, a long row of arithmetic, compiling) can run past it. Use
    /// [`QueryEngine::start`] where that matters, and this in tests or where
    /// there are no processes.
    ///
    /// Loading waits while a query or another load in this process runs,
    /// and holds the same lock; waiting and loading together are bounded by
    /// [`Limits::max_load_time`].
    pub fn load(inputs: &[TableInput], limits: Limits) -> Result<QueryEngine, QueryError> {
        Ok(QueryEngine {
            inner: Inner::Local(Local::load(inputs, limits)?),
        })
    }

    /// The loaded tables: SQL names, columns and their types.
    pub fn tables(&self) -> &[TableInfo] {
        match &self.inner {
            Inner::Local(l) => l.tables(),
            Inner::Remote(r) => r.tables(),
        }
    }

    /// Runs one read-only SQL statement and returns the page of its result
    /// that `page` asks for. For the next page, run the same query with
    /// `offset` set to the result's `next_offset`.
    pub fn query(&self, sql: &str, page: Page) -> Result<QueryResult, QueryError> {
        match &self.inner {
            Inner::Local(l) => l.query(sql, page),
            Inner::Remote(r) => r.query(sql, page),
        }
    }

    /// The worker's process id while one runs; `None` for an engine loaded
    /// in this process, or after a worker was killed. For tests.
    #[doc(hidden)]
    pub fn worker_id(&self) -> Option<u32> {
        match &self.inner {
            Inner::Local(_) => None,
            Inner::Remote(r) => r.pid(),
        }
    }
}
