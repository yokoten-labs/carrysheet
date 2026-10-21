//! Query results, pages and limits.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Bounds on loading and on every query. The defaults suit a command-line
/// tool on a small machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// Cells loaded over all tables. Default 5,000,000.
    pub max_input_cells: u64,
    /// SQLite virtual-machine steps per query, counted in blocks of 10.
    /// Bounds huge cross joins and runaway recursive queries. Default 250,000,000
    /// (about a second or two).
    pub max_steps: u64,
    /// Memory a query is promised beyond the loaded data, for sorts, GROUP BY,
    /// DISTINCT and temporary results. Default 256 MiB. A load is refused
    /// when the loaded data of every engine plus this does not fit in the
    /// budget. A running query may use more, up to what is left of the
    /// budget, because queries run one at a time.
    pub max_memory: u64,
    /// SQLite's memory budget for the whole process: all loaded data plus
    /// the running query. Default 512 MiB; 0 is refused. An engine from
    /// `QueryEngine::start` counts too, though its data is in a worker
    /// process: its data, the copy of the sheets kept to restart the worker,
    /// and its own `max_memory`. Only the first engine's value counts:
    /// SQLite's heap limit is process-wide and can only be lowered, so it is
    /// set once and never changed.
    pub memory_budget: u64,
    /// Wall-clock time per query, including time spent waiting while another
    /// query or load runs (they run one at a time). Default 10 seconds. A
    /// query in a worker process (`QueryEngine::start`) ends at most about
    /// 0.2 s after it, when the worker is killed; one loaded in this process
    /// can run past it while sorting or compiling (see the crate
    /// documentation).
    pub max_time: Duration,
    /// Wall-clock time to load, including waiting for a running query; a
    /// load holds the lock that queries wait for. Default 60 seconds.
    pub max_load_time: Duration,
    /// Length of the query text in bytes. Default 100,000.
    pub max_sql_bytes: usize,
    /// Bytes in any one value or row, loaded or made by a query. Default
    /// 256 KiB, which also bounds how long one call of a function such as
    /// LIKE can take: the progress handler cannot stop a call halfway.
    /// Loading refuses a cell or row that is larger.
    pub max_value_bytes: usize,
    /// Rows a page may hold, whatever the caller asks. Default 10,000.
    pub max_page_rows: usize,
    /// Cells (rows times columns) a page may hold; a page always has at least
    /// one row. Default 100,000.
    pub max_page_cells: usize,
    /// Text and blob bytes a page may hold; a page always has at least one row.
    /// Default 8 MiB.
    pub max_page_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_input_cells: 5_000_000,
            max_steps: 250_000_000,
            max_memory: 256 * 1024 * 1024,
            memory_budget: 512 * 1024 * 1024,
            max_time: Duration::from_secs(10),
            max_load_time: Duration::from_secs(60),
            max_sql_bytes: 100_000,
            max_value_bytes: 256 * 1024,
            max_page_rows: 10_000,
            max_page_cells: 100_000,
            max_page_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Which rows of the result to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// Result rows to skip: 0 for the first page, then `next_offset`.
    pub offset: u64,
    /// Rows to return at most (capped by [`Limits::max_page_rows`]). Default 200.
    pub max_rows: usize,
}

impl Default for Page {
    fn default() -> Self {
        Page {
            offset: 0,
            max_rows: 200,
        }
    }
}

/// One result column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultColumn {
    pub name: String,
    /// The declared type when the column is a table column, such as `INTEGER`
    /// or `DATE`; `None` for an expression such as `SUM(x)`.
    pub declared_type: Option<String>,
}

/// One page of a query's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<ResultColumn>,
    /// Typed values: numbers, text, `true`/`false` for BOOLEAN columns, and
    /// `null` for empty cells. A blob shows as `x'..'` hex text.
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Rows on this page.
    pub row_count: usize,
    /// The offset this page started at.
    pub offset: u64,
    /// True when more rows follow this page.
    pub truncated: bool,
    /// The offset for the next page, when `truncated`: run the same query
    /// again with this offset.
    pub next_offset: Option<u64>,
    /// Things to know about the values, such as how dates are stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The note shown when a result has dates.
pub(crate) const DATE_NOTE: &str = "Dates are ISO 8601 text (YYYY-MM-DD, or YYYY-MM-DDTHH:MM:SS with a time). Compare them as text, or use date(), strftime() and julianday().";
