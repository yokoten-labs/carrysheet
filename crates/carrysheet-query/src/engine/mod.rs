//! The query engine: loads tables into an in-memory SQLite and runs read-only
//! queries on them, a page at a time.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(crate) mod budget;
mod cost;
mod functions;
mod guard;
mod pattern;
mod scan;

pub use guard::{ALLOWED_FUNCTIONS, ALLOWED_READS};

use carrysheet_model::CellValue;
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Batch, Connection, ErrorCode};
use serde_json::Value as Json;

use crate::error::{QueryError, QueryErrorCode};
use crate::input::{
    Affinity, Names, QueryCell, TableInfo, TableInput, as_integer, plan_table, quote_ident, shown,
};
use crate::output::{DATE_NOTE, Limits, Page, QueryResult, ResultColumn};

/// The most columns one table can have (SQLite's built-in maximum).
const MAX_COLUMNS: usize = 2000;
/// Names listed at most in a "no such table/column" error.
const MAX_LISTED_NAMES: usize = 30;
/// Statements that change something, refused by their first word for a clear
/// message. The authorizer and `query_only` refuse them too (and any write
/// hidden after `WITH`).
const WRITE_KEYWORDS: &[&str] = &[
    "INSERT",
    "UPDATE",
    "DELETE",
    "REPLACE",
    "UPSERT",
    "CREATE",
    "DROP",
    "ALTER",
    "ATTACH",
    "DETACH",
    "VACUUM",
    "REINDEX",
    "ANALYZE",
    "BEGIN",
    "COMMIT",
    "END",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
];
/// Error-cell notes shown at most per result.
const MAX_ERROR_NOTES: usize = 5;
const COMPOUND_BOOLEAN_NOTE: &str = "This query combines SELECTs (UNION, INTERSECT or EXCEPT), so TRUE and FALSE come back as 1 and 0.";
const UNORDERED_NOTE: &str = "The query has no ORDER BY, so SQLite may return rows in a different order on another page. Add ORDER BY to page through the result reliably.";
/// Bytes of a blob shown at most, as hex.
const MAX_BLOB_SHOWN: usize = 64;

/// Loaded tables in a private, read-only, in-memory SQLite database in this
/// process: the engine behind [`crate::QueryEngine`], run either in the
/// caller's process or in a worker process.
pub(crate) struct Local {
    conn: Connection,
    tables: Vec<TableInfo>,
    limits: Limits,
    /// What the authorizer saw during the last prepare.
    seen: Arc<Mutex<guard::Seen>>,
    /// Why the progress handler stopped the last query.
    stopped: Arc<AtomicU8>,
    /// Set by the watchdog once the running query is past its time limit;
    /// our functions read it too.
    late: Arc<AtomicBool>,
    /// What the loaded data takes in SQLite, measured after loading.
    data_bytes: u64,
    /// This engine's share of the process's memory budget.
    _reservation: budget::Reservation,
}

impl Local {
    /// Loads the tables and locks the database. Each input becomes one table;
    /// see [`Local::tables`] for the names and types it got.
    ///
    /// Loading waits while a query or another load runs, and holds the same
    /// lock; waiting and loading together are bounded by
    /// [`Limits::max_load_time`].
    pub(crate) fn load(inputs: &[TableInput], limits: Limits) -> Result<Local, QueryError> {
        let start = Instant::now();
        check_size(inputs, &limits)?;
        let mut taken = Names::default();
        let tables: Vec<TableInfo> = inputs
            .iter()
            .enumerate()
            .map(|(i, input)| plan_table(input, i, &mut taken))
            .collect();

        let reservation = budget::Reservation::new(estimate_bytes(inputs), &limits)?;
        // Loading allocates under the shared heap limit too.
        let deadline = start + limits.max_load_time;
        let Some(_running) = budget::run_lock(deadline) else {
            return Err(load_too_slow(&limits, true));
        };
        let mut conn = Connection::open_in_memory().map_err(internal)?;
        guard::harden(&conn).map_err(internal)?;
        let tx = conn.transaction().map_err(internal)?;
        for (input, info) in inputs.iter().zip(&tables) {
            match load_table(&tx, input, info, deadline) {
                Ok(true) => {}
                Ok(false) => return Err(load_too_slow(&limits, false)),
                Err(e) => return Err(load_error(&info.name, e)),
            }
        }
        tx.commit().map_err(internal)?;
        let data_bytes = measured_bytes(&conn).map_err(internal)?;
        reservation.update(data_bytes)?;

        let seen = Arc::new(Mutex::new(guard::Seen::default()));
        let names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
        let late = Arc::new(AtomicBool::new(false));
        guard::lock(&conn, &limits, &names, Arc::clone(&seen), &late).map_err(internal)?;
        Ok(Local {
            conn,
            tables,
            limits,
            seen,
            stopped: Arc::new(AtomicU8::new(guard::STOPPED_NONE)),
            late,
            data_bytes,
            _reservation: reservation,
        })
    }

    /// The loaded tables: SQL names, columns and their types.
    pub(crate) fn tables(&self) -> &[TableInfo] {
        &self.tables
    }

    /// What the loaded data takes in SQLite, measured after loading.
    pub(crate) fn data_bytes(&self) -> u64 {
        self.data_bytes
    }

    /// Runs one read-only SQL statement and returns the page of its result
    /// that `page` asks for. For the next page, run the same query with
    /// `offset` set to the result's `next_offset`.
    pub(crate) fn query(&self, sql: &str, page: Page) -> Result<QueryResult, QueryError> {
        let start = Instant::now();
        if is_blank_sql(sql) {
            return Err(self.empty_query());
        }
        if sql.len() > self.limits.max_sql_bytes {
            return Err(QueryError::new(
                QueryErrorCode::TooLarge,
                format!("the query is {} bytes long", sql.len()),
                format!(
                    "Queries may be at most {} bytes, to keep them fast to check.",
                    self.limits.max_sql_bytes
                ),
                "Shorten the query, for example with a CTE (WITH ...) instead of repeated parts.",
            ));
        }
        // A first, clear refusal; the authorizer and query_only stay the real guard.
        let keyword = first_keyword(sql);
        if WRITE_KEYWORDS.contains(&keyword.as_str()) {
            return Err(self.not_allowed(&keyword));
        }
        let scan = scan::scan(sql);
        if let Some((name, count)) = scan.long_call {
            return Err(too_many_arguments(name, count));
        }
        let offset = page.offset;
        // One query at a time, so it has the whole memory budget's room.
        // Waiting counts toward max_time.
        let Some(_running) = budget::run_lock(start + self.limits.max_time) else {
            return Err(QueryError::new(
                QueryErrorCode::TimeLimit,
                format!(
                    "the query did not start within {} ms",
                    self.limits.max_time.as_millis()
                ),
                "Another query or a load was running the whole time; queries run one at a time, and waiting counts toward the time limit.",
                "Try again when the other query has finished.",
            ));
        };
        if let Ok(mut seen) = self.seen.lock() {
            *seen = guard::Seen::default();
        }
        // Batch prepares one statement at a time, without recursion, so a
        // long run of `SELECT 1;` cannot exhaust the stack.
        let mut batch = Batch::new(&self.conn, sql);
        let mut stmt = match batch.next() {
            Ok(Some(stmt)) => stmt,
            Ok(None) => return Err(self.empty_query()),
            Err(e) if e.sqlite_error_code() == Some(ErrorCode::OutOfMemory) => {
                return Err(too_large_to_compile());
            }
            Err(e) => return Err(self.sql_error(e, sql, offset)),
        };
        match batch.next() {
            Ok(None) => {}
            Ok(Some(_)) => return Err(multiple_statements()),
            Err(e) => return Err(self.sql_error(e, sql, offset)),
        }
        if !stmt.readonly() {
            return Err(self.not_allowed("a statement that changes the database"));
        }
        // What one row can cost, from the compiled program. An EXPLAIN only
        // lists a program and runs nothing.
        if stmt.is_explain() == 0 {
            let reads = self
                .seen
                .lock()
                .map(|s| s.reads.clone())
                .unwrap_or_default();
            let cost =
                cost::measure(&self.conn, sql).map_err(|e| self.sql_error(e, sql, offset))?;
            if let Ok(mut seen) = self.seen.lock() {
                seen.reads = reads;
            }
            if cost.too_much() {
                return Err(too_much_work(&cost));
            }
        }
        let columns: Vec<ResultColumn> = stmt
            .columns()
            .iter()
            .map(|c| ResultColumn {
                name: c.name().to_owned(),
                declared_type: c.decl_type().map(str::to_owned),
            })
            .collect();
        let mut notes = Vec::new();
        let any_boolean = columns.iter().any(|c| has_type(c, "BOOLEAN"));
        // A combined SELECT takes its declared types from the first part, so a
        // BOOLEAN column there may hold plain numbers from the other parts.
        let booleans: Vec<bool> = columns
            .iter()
            .map(|c| !scan.compound && has_type(c, "BOOLEAN"))
            .collect();
        if any_boolean && scan.compound {
            notes.push(COMPOUND_BOOLEAN_NOTE.to_owned());
        }
        if columns.iter().any(|c| has_type(c, "DATE")) {
            notes.push(DATE_NOTE.to_owned());
        }
        if scan.aggregate {
            notes.extend(self.error_cell_notes());
        }

        let width = columns.len().max(1);
        let max_rows = page
            .max_rows
            .min(self.limits.max_page_rows)
            .min(self.limits.max_page_cells / width)
            .max(1);

        let watchdog = guard::watch(
            &self.conn,
            &self.limits,
            start,
            Arc::clone(&self.stopped),
            &self.late,
        )
        .map_err(internal)?;
        let collected = self.collect(&mut stmt, page.offset, max_rows, &booleans);
        let _ = guard::unwatch(&self.conn);
        drop(watchdog);
        let (rows, more) = collected.map_err(|e| self.sql_error(e, sql, offset))?;

        let row_count = rows.len();
        let next_offset = more.then(|| page.offset + row_count as u64);
        if more {
            notes.push(format!(
                "More rows follow. Run the same query with offset {} for the next page.",
                page.offset + row_count as u64
            ));
        }
        if (more || offset > 0) && !scan.order_by {
            notes.push(UNORDERED_NOTE.to_owned());
        }
        Ok(QueryResult {
            columns,
            rows,
            row_count,
            offset: page.offset,
            truncated: more,
            next_offset,
            notes,
        })
    }

    /// Steps the statement: skips `offset` rows, keeps up to `max_rows` (and
    /// within the byte budget), then looks one row ahead to see if more follow.
    fn collect(
        &self,
        stmt: &mut rusqlite::Statement<'_>,
        offset: u64,
        max_rows: usize,
        booleans: &[bool],
    ) -> rusqlite::Result<(Vec<Vec<Json>>, bool)> {
        let mut result = stmt.query([])?;
        let mut skipped = 0u64;
        while skipped < offset {
            if result.next()?.is_none() {
                return Ok((Vec::new(), false));
            }
            skipped += 1;
        }
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = result.next()? {
            if rows.len() >= max_rows || (bytes >= self.limits.max_page_bytes && !rows.is_empty()) {
                return Ok((rows, true));
            }
            let mut values = Vec::with_capacity(booleans.len());
            for (i, &boolean) in booleans.iter().enumerate() {
                let (value, size) = to_json(row.get_ref(i)?, boolean);
                bytes = bytes.saturating_add(size);
                values.push(value);
            }
            rows.push(values);
        }
        Ok((rows, false))
    }

    /// An example query on the first table.
    fn example(&self) -> String {
        match self.tables.first() {
            Some(t) => format!("SELECT * FROM {} LIMIT 10", quote_ident(&t.name)),
            None => "SELECT 1".to_owned(),
        }
    }

    fn empty_query(&self) -> QueryError {
        QueryError::new(
            QueryErrorCode::EmptyQuery,
            "the query is empty",
            "There is no SQL statement to run, only spaces or comments.",
            format!("Write a SELECT query, for example: {}", self.example()),
        )
    }

    /// Notes for the columns the query read that hold error cells.
    fn error_cell_notes(&self) -> Vec<String> {
        let Ok(seen) = self.seen.lock() else {
            return Vec::new();
        };
        let mut notes: Vec<String> = Vec::new();
        for (table, column) in &seen.reads {
            let Some(info) = self
                .tables
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(table))
            else {
                continue;
            };
            let Some(col) = info
                .columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(column) && c.errors > 0)
            else {
                continue;
            };
            let note = format!(
                "Column {} of {} has {} error cells (such as #N/A), stored as text: SUM and AVG count them as 0, COUNT counts them. Leave them out with WHERE typeof({}) <> 'text' when the column holds numbers.",
                quote_ident(&col.name),
                quote_ident(&info.name),
                col.errors,
                quote_ident(&col.name)
            );
            if !notes.contains(&note) && notes.len() < MAX_ERROR_NOTES {
                notes.push(note);
            }
        }
        notes
    }

    fn not_allowed(&self, what: &str) -> QueryError {
        QueryError::new(
            QueryErrorCode::NotAllowed,
            format!("{what} is not allowed"),
            "Queries are read-only: only SELECT (with WITH, JOIN, GROUP BY and so on) runs on the loaded sheets. Writing, ATTACH, extensions and settings are refused.",
            format!(
                "Write a SELECT query, for example: {}. To change cells, use the edit command.",
                self.example()
            ),
        )
    }

    /// Turns a SQLite error into what/why/fix.
    fn sql_error(&self, err: rusqlite::Error, sql: &str, offset: u64) -> QueryError {
        let (code, message) = match &err {
            rusqlite::Error::SqliteFailure(e, msg) => {
                (Some(e.code), msg.clone().unwrap_or_else(|| e.to_string()))
            }
            // A prepare error: only SQLite's message, never the SQL itself.
            rusqlite::Error::SqlInputError { error, msg, .. } => (Some(error.code), msg.clone()),
            rusqlite::Error::MultipleStatement => return multiple_statements(),
            other => (None, other.to_string()),
        };
        // SQLite quotes the token it stopped at, which may be a long literal:
        // the message goes into errors shortened.
        let short_message = shorten(&message);
        // The authorizer refused something. SQLite reports a refused function
        // as a plain error, so look at what was refused, not only the code.
        match self.seen.lock().ok().and_then(|s| s.denied.clone()) {
            Some(guard::Denied::Statement(what)) => return self.not_allowed(&what),
            Some(guard::Denied::Function(name)) => return function_not_allowed(&name),
            Some(guard::Denied::Read(table)) => return read_not_allowed(&table),
            None => {}
        }
        match code {
            Some(ErrorCode::AuthorizationForStatementDenied) => {
                return self.not_allowed("this statement");
            }
            Some(ErrorCode::ReadOnly) => return self.not_allowed("writing"),
            Some(ErrorCode::OperationInterrupted) => return self.stopped_error(offset),
            Some(ErrorCode::OutOfMemory) => {
                return QueryError::new(
                    QueryErrorCode::TooLarge,
                    "the query needs more memory than allowed",
                    "Sorting, grouping and temporary results may use what is left of the memory budget after the loaded sheets (queries run one at a time).",
                    "Narrow the query: filter rows with WHERE before ORDER BY, GROUP BY or DISTINCT, and select fewer or shorter columns.",
                );
            }
            Some(ErrorCode::TooBig) => {
                return QueryError::new(
                    QueryErrorCode::TooLarge,
                    format!("the query made a value that is too large ({short_message})"),
                    format!(
                        "A value or row may be at most {} bytes.",
                        self.limits.max_value_bytes
                    ),
                    "Make the result smaller: select fewer or shorter columns, or build less text in one value (for example group_concat over fewer rows).",
                );
            }
            _ => {}
        }
        // The message may go on with " in <sql> at offset N".
        if let Some(rest) = message.strip_prefix("no such function: ")
            && let (name, _) = split_name(rest)
            && let Some(f) = guard::DENIED_FUNCTIONS
                .iter()
                .find(|f| f.eq_ignore_ascii_case(name))
        {
            // Left out of this SQLite build; refused either way.
            return function_not_allowed(f);
        }
        if let Some(rest) = message.strip_prefix("no such function: ") {
            let (name, _) = split_name(rest);
            return QueryError::new(
                QueryErrorCode::Sql,
                format!("there is no function named {}", shown(name)),
                "SQLite does not have this function, or not with this number of arguments.",
                "Use an allowed function (see ALLOWED_FUNCTIONS in carrysheet-query's documentation), for example SUM, COUNT, ROUND, SUBSTR, LOWER, UPPER or STRFTIME.",
            );
        }
        if message.contains("pattern too complex") {
            return QueryError::new(
                QueryErrorCode::TooLarge,
                "the LIKE or GLOB pattern is too long",
                format!(
                    "Patterns may be at most {} characters, because matching a long pattern against long text cannot be stopped once it starts.",
                    guard::MAX_LIKE_PATTERN
                ),
                "Use a shorter pattern, or compare with = or instr() instead.",
            );
        }
        if message.contains("may not be modified") {
            return self.not_allowed("changing the database schema");
        }
        if let Some(rest) = message.strip_prefix("no such table: ") {
            let (name, _) = split_name(rest);
            return QueryError::new(
                QueryErrorCode::NoSuchTable,
                format!("there is no table named {}", shown(name)),
                format!("The tables you can query are: {}.", self.table_list()),
                "Use one of those names. Put names with spaces, symbols or non-English letters in double quotes, for example: SELECT * FROM \"Sales 2024\"",
            );
        }
        if let Some(rest) = message.strip_prefix("no such column: ") {
            let (name, hint) = split_name(rest);
            // SQLite's own hint, such as "should this be a string literal in
            // single-quotes?", goes first in the fix.
            let hint = hint
                .map(|h| format!("{} ", capitalize(h)))
                .unwrap_or_default();
            return QueryError::new(
                QueryErrorCode::NoSuchColumn,
                format!("there is no column named {}", shown(name)),
                format!("The columns are: {}.", self.column_list(sql)),
                format!(
                    "{hint}Use one of those names, in double quotes when it has spaces, symbols or non-English letters: SELECT \"Unit price\" FROM ... Text values go in single quotes: WHERE region = 'East'."
                ),
            );
        }
        if message.contains("syntax error")
            || message.contains("incomplete input")
            || message.contains("unrecognized token")
        {
            return QueryError::new(
                QueryErrorCode::Syntax,
                format!("the SQL could not be read: {short_message}"),
                "The query is not valid SQLite SQL.",
                format!(
                    "Check the part the message points to. Names go in double quotes, text in single quotes, for example: {}",
                    self.example()
                ),
            );
        }
        if let Some(rest) = message.strip_prefix("too many arguments on function ") {
            let (name, _) = split_name(rest);
            return QueryError::new(
                QueryErrorCode::TooLarge,
                format!("the function {} has too many arguments", shown(name)),
                "SQLite allows at most 1,000 arguments in one function call.",
                "Split the call into several smaller ones, or build the value over several queries.",
            );
        }
        if message.contains("Expression tree is too large") {
            return QueryError::new(
                QueryErrorCode::TooLarge,
                "an expression in the query is nested too deeply",
                "An expression may be at most 500 levels deep; a long chain such as a OR b OR c ... counts one level per part.",
                "Use IN (...) instead of a long OR chain, or split the expression over several queries.",
            );
        }
        if message.contains("too many columns") {
            return QueryError::new(
                QueryErrorCode::TooLarge,
                format!("the result has too many columns ({short_message})"),
                format!("A result may have at most {MAX_COLUMNS} columns."),
                "Select fewer columns instead of SELECT *.",
            );
        }
        let (code, why, fix) = sql_reason(&message).unwrap_or((
            QueryErrorCode::Sql,
            "SQLite could not run the query.",
            "Check the query; list a table's columns with PRAGMA table_info(\"Sheet1\").",
        ));
        QueryError::new(code, format!("the query failed: {short_message}"), why, fix)
    }

    fn stopped_error(&self, offset: u64) -> QueryError {
        let again = if offset > 0 {
            " Each page runs the query again from the start and skips the rows before it, so a later page costs more."
        } else {
            ""
        };
        if self.stopped.load(Ordering::Relaxed) == guard::STOPPED_TIME
            || self.late.load(Ordering::Relaxed)
        {
            time_limit_error(&self.limits, offset)
        } else {
            QueryError::new(
                QueryErrorCode::StepLimit,
                format!(
                    "the query was stopped after about {} steps",
                    self.limits.max_steps
                ),
                format!(
                    "Queries may do a limited amount of work, which bounds huge cross joins and endless recursive queries.{again}"
                ),
                "Narrow the query: add WHERE conditions, join with ON conditions, or give a recursive WITH a stop condition.",
            )
        }
    }

    /// The table names, quoted, at most [`MAX_LISTED_NAMES`].
    fn table_list(&self) -> String {
        if self.tables.is_empty() {
            return "none (no sheets or tables were loaded)".to_owned();
        }
        bounded_list(self.tables.iter().map(|t| quote_ident(&t.name)))
    }

    /// The columns of the tables the query mentions (or of all tables), as
    /// `"Table"("A", "B")`, at most [`MAX_LISTED_NAMES`] names in all.
    fn column_list(&self, sql: &str) -> String {
        let lower = sql.to_lowercase();
        let mentioned: Vec<&TableInfo> = self
            .tables
            .iter()
            .filter(|t| lower.contains(&t.name.to_lowercase()))
            .collect();
        let shown: Vec<&TableInfo> = if mentioned.is_empty() {
            self.tables.iter().collect()
        } else {
            mentioned
        };
        let mut out = String::new();
        let mut budget = MAX_LISTED_NAMES;
        for (n, t) in shown.iter().enumerate() {
            if budget == 0 {
                let _ = write!(out, "; and {} more tables", shown.len() - n);
                break;
            }
            if !out.is_empty() {
                out.push_str("; ");
            }
            let take = t.columns.len().min(budget);
            budget -= take;
            let mut names = t.columns[..take]
                .iter()
                .map(|c| quote_ident(&c.name))
                .collect::<Vec<_>>()
                .join(", ");
            if take < t.columns.len() {
                let _ = write!(names, ", and {} more", t.columns.len() - take);
            }
            let _ = write!(out, "{}({names})", quote_ident(&t.name));
        }
        if out.is_empty() {
            "none (no sheets or tables were loaded)".to_owned()
        } else {
            out
        }
    }
}

/// True when the column's declared type is `ty`.
fn has_type(column: &ResultColumn, ty: &str) -> bool {
    column
        .declared_type
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case(ty))
}

/// The SQL after any leading spaces and comments.
fn skip_comments(sql: &str) -> &str {
    let mut rest = sql.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, r)| r).trim_start();
        } else {
            return rest;
        }
    }
}

/// True when the SQL holds only spaces, comments and semicolons.
fn is_blank_sql(sql: &str) -> bool {
    skip_comments(sql).trim_start_matches(';').trim().is_empty()
}

/// The statement's first word in capitals, such as `SELECT`; empty when it
/// does not start with a letter.
fn first_keyword(sql: &str) -> String {
    skip_comments(sql)
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .take(16)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// `"A", "B", "C", and 12 more`.
fn bounded_list(names: impl ExactSizeIterator<Item = String>) -> String {
    let total = names.len();
    let mut list: Vec<String> = names.take(MAX_LISTED_NAMES).collect();
    if total > MAX_LISTED_NAMES {
        list.push(format!("and {} more", total - MAX_LISTED_NAMES));
    }
    list.join(", ")
}

/// The error for a query stopped at its time limit, by SQLite's progress
/// handler or by killing the worker process.
pub(crate) fn time_limit_error(limits: &Limits, offset: u64) -> QueryError {
    let again = if offset > 0 {
        " Each page runs the query again from the start and skips the rows before it, so a later page costs more."
    } else {
        ""
    };
    QueryError::new(
        QueryErrorCode::TimeLimit,
        format!(
            "the query was stopped after {} ms",
            limits.max_time.as_millis()
        ),
        format!(
            "Queries may run for a limited time, so a runaway query cannot hang the tool.{again}"
        ),
        "Narrow the query: add WHERE conditions or a JOIN ... ON condition instead of joining every row with every row.",
    )
}

/// Refuses inputs that are too wide or too big before anything is planned.
pub(crate) fn check_size(inputs: &[TableInput], limits: &Limits) -> Result<(), QueryError> {
    let mut cells = 0u64;
    for input in inputs {
        let width = input.width();
        if width > MAX_COLUMNS {
            return Err(QueryError::new(
                QueryErrorCode::InvalidInput,
                format!(
                    "{} has {width} columns, too many to query",
                    shown(&input.name),
                ),
                format!("A table for queries can have at most {MAX_COLUMNS} columns."),
                "Query a narrower range of the sheet.",
            ));
        }
        cells = cells.saturating_add(input.rows.iter().map(|r| r.len() as u64).sum());
        check_values(input, limits.max_value_bytes)?;
    }
    if cells > limits.max_input_cells {
        return Err(QueryError::new(
            QueryErrorCode::InvalidInput,
            format!("the sheets hold {cells} cells, too many to load for a query"),
            format!(
                "At most {} cells are loaded, to keep memory use bounded.",
                limits.max_input_cells
            ),
            "Query fewer sheets or a smaller range.",
        ));
    }
    Ok(())
}

/// Refuses a cell or a row larger than `max` bytes: SQLite's length limit
/// covers both, and a query on them would fail or be slow.
fn check_values(input: &TableInput, max: usize) -> Result<(), QueryError> {
    let start = input.data_start();
    for (r, row) in input.data_rows().iter().enumerate() {
        let mut row_bytes = 0usize;
        for (c, cell) in row.iter().enumerate() {
            let len = match cell {
                QueryCell::Value(CellValue::Text(s)) | QueryCell::Date(s) => s.len(),
                _ => 0,
            };
            if len > max {
                return Err(QueryError::new(
                    QueryErrorCode::InvalidInput,
                    format!(
                        "cell {} of {} holds {len} bytes of text, more than a query value may hold",
                        input.address(start + r, c),
                        shown(&input.name)
                    ),
                    format!(
                        "Values are limited to {max} bytes (Limits::max_value_bytes), so that one function call on a value stays fast."
                    ),
                    "Query a range that leaves this cell out, or raise max_value_bytes.",
                ));
            }
            // An upper bound of SQLite's record: the text plus up to 9 bytes
            // per cell for its type and number.
            row_bytes = row_bytes.saturating_add(len + 9);
        }
        if row_bytes > max {
            return Err(QueryError::new(
                QueryErrorCode::InvalidInput,
                format!(
                    "row {} of {} holds about {row_bytes} bytes, more than a query row may hold",
                    input
                        .address(start + r, 0)
                        .trim_start_matches(|c: char| c.is_ascii_alphabetic()),
                    shown(&input.name)
                ),
                format!(
                    "Rows are limited to {max} bytes (Limits::max_value_bytes), the same as values, because SQLite sorts and groups whole rows."
                ),
                "Query fewer columns, or a range that leaves this row out.",
            ));
        }
    }
    Ok(())
}

/// Roughly the heap the loaded data will take in SQLite, before loading:
/// about twice the raw values, for row headers, B-tree pages and slack.
pub(crate) fn estimate_bytes(inputs: &[TableInput]) -> u64 {
    let raw: u64 = inputs
        .iter()
        .flat_map(|t| &t.rows)
        .flatten()
        .map(|cell| match cell {
            QueryCell::Value(CellValue::Text(s)) | QueryCell::Date(s) => s.len() as u64 + 8,
            _ => 10,
        })
        .sum();
    raw.saturating_mul(2)
        .saturating_add(budget::ENGINE_OVERHEAD)
}

/// The heap the loaded database takes: its pages, plus about a tenth for the
/// page cache's own records, plus the connection.
fn measured_bytes(conn: &Connection) -> rusqlite::Result<u64> {
    let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let bytes = u64::try_from(pages.saturating_mul(size)).unwrap_or(0);
    Ok(bytes + bytes / 10 + budget::ENGINE_OVERHEAD)
}

/// Splits SQLite's "name - hint" (or an older "name in <sql> at offset N")
/// into the name and the hint.
fn split_name(rest: &str) -> (&str, Option<&str>) {
    if let Some((name, hint)) = rest.split_once(" - ") {
        // With the single-quote hint SQLite puts the name in double quotes.
        let name = name
            .strip_prefix('"')
            .and_then(|n| n.strip_suffix('"'))
            .unwrap_or(name);
        return (name, Some(hint));
    }
    if rest.contains(" at offset ")
        && let Some((name, _)) = rest.split_once(" in ")
    {
        return (name, None);
    }
    (rest, None)
}

/// An SQLite message as errors show it: every word longer than 64
/// characters (a name or token from the query) cut like [`shown`], then at
/// most 200 characters in all.
fn shorten(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(256));
    let mut word = String::new();
    for c in message.chars() {
        if c.is_whitespace() {
            out.push_str(&cut_word(&word));
            word.clear();
            out.push(c);
        } else {
            word.push(c);
        }
    }
    out.push_str(&cut_word(&word));
    match out.char_indices().nth(200) {
        Some((end, _)) => format!("{}...", &out[..end]),
        None => out,
    }
}

/// A word cut to 64 characters plus "...", keeping the closing quotes and
/// punctuation: `"xxx…x":` becomes `"xxx…x...":`.
fn cut_word(word: &str) -> String {
    const MAX: usize = 64;
    if word.chars().count() <= MAX {
        return word.to_owned();
    }
    let mut tail: Vec<char> = word
        .chars()
        .rev()
        .take_while(|c| matches!(c, '"' | '\'' | ':' | ',' | ')' | ']' | '`'))
        .take(3)
        .collect();
    tail.reverse();
    let head: String = word.chars().take(MAX).collect();
    format!("{head}...{}", tail.into_iter().collect::<String>())
}

/// Why SQLite refused a query, and how to fix it, from its message.
fn sql_reason(message: &str) -> Option<(QueryErrorCode, &'static str, &'static str)> {
    type Reason = (
        &'static [&'static str],
        QueryErrorCode,
        &'static str,
        &'static str,
    );
    const REASONS: &[Reason] = &[
        (
            &["integer overflow"],
            QueryErrorCode::Sql,
            "A whole number went past SQLite's range (about 9.2 billion billion either way).",
            "Work in real numbers instead: multiply by 1.0 first, for example a * 1.0 * b, or use CAST(a AS REAL).",
        ),
        (
            &["malformed JSON", "bad JSON path", "JSON cannot hold BLOB"],
            QueryErrorCode::Sql,
            "A JSON value or path in the query is not valid.",
            "Skip text that is not JSON with WHERE json_valid(doc), and write paths like '$.name' or '$.items[0]'.",
        ),
        (
            &[
                "values for",
                "same number of result columns",
                "row value misused",
                "sub-select returns",
            ],
            QueryErrorCode::Sql,
            "The number of values does not match the number of columns.",
            "Give a WITH table as many column names as its SELECT has columns, make every SELECT in a UNION return the same number of columns, and compare one value with one value.",
        ),
        (
            &["tables in a join"],
            QueryErrorCode::TooLarge,
            "SQLite joins at most 64 tables in one SELECT.",
            "Join fewer tables at once: compute part of the join in a WITH table marked AS MATERIALIZED, or use several queries.",
        ),
        (
            &["too many FROM clause terms"],
            QueryErrorCode::TooLarge,
            "A query may read at most 200 tables in all, and each use of a WITH table counts the tables it reads again: a chain of WITH tables that each use the one before twice doubles at every step.",
            "Use each WITH table once (join it to itself through another WITH table if needed), or split the chain over several queries.",
        ),
        (
            &["ambiguous column name"],
            QueryErrorCode::Sql,
            "More than one table in the query has a column with this name.",
            "Put the table name or its alias in front of the column, for example: SELECT s.region FROM Sales s JOIN ...",
        ),
        (
            &["no such window"],
            QueryErrorCode::Sql,
            "OVER names a window that no WINDOW clause defines.",
            "Define it after GROUP BY (or WHERE): WINDOW w AS (PARTITION BY region ORDER BY day), or write the window in OVER (...) itself.",
        ),
        (
            &["no such collation sequence"],
            QueryErrorCode::Sql,
            "SQLite has only the BINARY, NOCASE and RTRIM collations.",
            "Use COLLATE NOCASE to ignore the case of English letters, or compare lower(a) = lower(b).",
        ),
        (
            &[
                "misuse of aggregate",
                "aggregate functions are not allowed",
                "misuse of window function",
            ],
            QueryErrorCode::Sql,
            "Aggregate and window functions such as SUM, COUNT and row_number() work over groups of rows, so they cannot appear in WHERE or GROUP BY, or inside each other.",
            "Filter groups with HAVING, for example GROUP BY region HAVING count(*) > 1, or compute the aggregate in a subquery first.",
        ),
        (
            &["term out of range"],
            QueryErrorCode::Sql,
            "ORDER BY 2 or GROUP BY 2 means the second result column, and the query has fewer columns.",
            "Use a number from 1 to the number of result columns, or name the column instead.",
        ),
        (
            &["ESCAPE expression"],
            QueryErrorCode::Sql,
            "The ESCAPE of a LIKE must be exactly one character.",
            "Write one character, for example: WHERE code LIKE '%10!%%' ESCAPE '!'",
        ),
    ];
    REASONS
        .iter()
        .find(|(parts, ..)| parts.iter().any(|p| message.contains(p)))
        .map(|&(_, code, why, fix)| (code, why, fix))
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn function_not_allowed(name: &str) -> QueryError {
    QueryError::new(
        QueryErrorCode::NotAllowed,
        format!("the function {name}() is not allowed in queries"),
        "Queries may call only functions that stay fast on large values; this one can take too long on large values, or is not needed to read sheets. The list is in carrysheet-query's documentation (ALLOWED_FUNCTIONS).",
        "Use an allowed function, for example SUM, COUNT, AVG, MIN, MAX, ROUND, SUBSTR, INSTR, REPLACE, LOWER, UPPER, DATE, STRFTIME or JSON_EXTRACT, or do this step outside the query.",
    )
}

fn read_not_allowed(table: &str) -> QueryError {
    QueryError::new(
        QueryErrorCode::NotAllowed,
        format!("reading {} is not allowed in queries", shown(table)),
        "Queries may read the loaded sheets and tables, sqlite_schema, json_each and pragma_table_info; other built-in tables can be slow or are not needed.",
        "Read a loaded sheet or table instead; list them with SELECT name FROM sqlite_schema.",
    )
}

fn too_much_work(cost: &cost::Cost) -> QueryError {
    let kind = cost.largest();
    QueryError::new(
        QueryErrorCode::TooLarge,
        format!(
            "the query does too much work in one step: {} {}",
            cost.count(kind),
            kind.describe()
        ),
        format!(
            "SQLite checks the time limit between rows, not inside one row's expressions, so the work of one row is limited: at most about {} {} alone, fewer when mixed with other work. Subqueries and WITH tables count once for every place that uses them.",
            kind.cap(),
            kind.describe()
        ),
        "Split the query into smaller ones, or work a value out once: a WITH table marked AS MATERIALIZED is computed once instead of being copied into every place that uses it.",
    )
}

fn too_many_arguments(name: &str, count: usize) -> QueryError {
    let fix = match name {
        "json_extract" => "Split the paths over several calls, for example json_extract(doc, '$.a', '$.b'), json_extract(doc, '$.c', '$.d'), or use doc ->> '$.a' for each path.".to_owned(),
        _ => format!("Nest the call, for example {name}({name}(a, b, c), {name}(d, e, f)), with at most {} arguments in each.", cost::MAX_ARGUMENTS),
    };
    QueryError::new(
        QueryErrorCode::TooLarge,
        format!("{name}() is called with {count} arguments"),
        format!(
            "{name}() may have at most {} arguments: it reads a whole value for each one, so a long call is slow on long text or JSON.",
            cost::MAX_ARGUMENTS
        ),
        fix,
    )
}

fn too_large_to_compile() -> QueryError {
    QueryError::new(
        QueryErrorCode::TooLarge,
        "the query is too large to compile",
        format!(
            "A query may compile to at most {} instructions, and compiling may use only what is left of the memory budget. Subqueries and WITH tables are copied into every place that uses them, so a short query can compile to a huge one.",
            cost::MAX_INSTRUCTIONS
        ),
        "Split the query into smaller ones, or mark a WITH table AS MATERIALIZED so it is computed once instead of being copied.",
    )
}

fn load_too_slow(limits: &Limits, waiting: bool) -> QueryError {
    QueryError::new(
        QueryErrorCode::TimeLimit,
        format!(
            "loading the sheets for queries took longer than {} ms",
            limits.max_load_time.as_millis()
        ),
        if waiting {
            "Another query or load was running the whole time; they run one at a time."
        } else {
            "The sheets are too large to load in the time allowed."
        },
        "Query fewer sheets or a smaller range, or try again when other queries have finished.",
    )
}

fn multiple_statements() -> QueryError {
    QueryError::new(
        QueryErrorCode::MultipleStatements,
        "the query holds more than one statement",
        "One call runs one statement, so a query cannot hide a second one after a `;`.",
        "Send one SELECT statement per call.",
    )
}

/// Creates one table and inserts its rows. `Ok(false)` when `deadline`
/// passed before all rows were in.
fn load_table(
    conn: &Connection,
    input: &TableInput,
    info: &TableInfo,
    deadline: Instant,
) -> rusqlite::Result<bool> {
    let table = quote_ident(&info.name);
    let defs: Vec<String> = info
        .columns
        .iter()
        .map(|c| format!("{} {}", quote_ident(&c.name), c.affinity.sql_type()))
        .map(|d| d.trim_end().to_owned())
        .collect();
    conn.execute_batch(&format!("CREATE TABLE {table} ({})", defs.join(", ")))?;
    let marks = vec!["?"; info.columns.len()].join(", ");
    let mut insert = conn.prepare(&format!("INSERT INTO {table} VALUES ({marks})"))?;
    let mut values = Vec::with_capacity(info.columns.len());
    for (n, row) in input.data_rows().iter().enumerate() {
        if n % 1000 == 0 && Instant::now() > deadline {
            return Ok(false);
        }
        values.clear();
        for (i, column) in info.columns.iter().enumerate() {
            values.push(to_sql(row.get(i), column.affinity));
        }
        insert.execute(rusqlite::params_from_iter(values.iter()))?;
    }
    Ok(true)
}

/// The value stored for a cell. Each cell keeps its own type; numbers are
/// stored as integers when whole, except in REAL columns.
fn to_sql(cell: Option<&QueryCell>, affinity: Affinity) -> Value {
    match cell {
        None | Some(QueryCell::Value(CellValue::Empty)) => Value::Null,
        Some(QueryCell::Value(CellValue::Number(n))) => match as_integer(*n) {
            Some(i) if affinity != Affinity::Real => Value::Integer(i),
            _ if n.is_finite() => Value::Real(*n),
            _ => Value::Null,
        },
        Some(QueryCell::Value(CellValue::Text(s))) => Value::Text(s.clone()),
        Some(QueryCell::Value(CellValue::Bool(b))) => Value::Integer(i64::from(*b)),
        Some(QueryCell::Value(CellValue::Error(e))) => Value::Text(e.as_str().to_owned()),
        Some(QueryCell::Date(d)) => Value::Text(d.clone()),
    }
}

/// A result value as JSON, and roughly how many bytes it adds to the page.
fn to_json(value: ValueRef<'_>, boolean: bool) -> (Json, usize) {
    match value {
        ValueRef::Null => (Json::Null, 4),
        ValueRef::Integer(i) if boolean && (i == 0 || i == 1) => (Json::Bool(i == 1), 5),
        ValueRef::Integer(i) => (Json::from(i), 8),
        ValueRef::Real(f) => (
            serde_json::Number::from_f64(f).map_or(Json::Null, Json::Number),
            8,
        ),
        ValueRef::Text(t) => (
            Json::String(String::from_utf8_lossy(t).into_owned()),
            t.len(),
        ),
        ValueRef::Blob(b) => {
            let mut hex = String::with_capacity(4 + 2 * b.len().min(MAX_BLOB_SHOWN));
            hex.push_str("x'");
            for byte in b.iter().take(MAX_BLOB_SHOWN) {
                let _ = write!(hex, "{byte:02X}");
            }
            hex.push('\'');
            if b.len() > MAX_BLOB_SHOWN {
                let _ = write!(hex, " ({} bytes)", b.len());
            }
            let size = hex.len();
            (Json::String(hex), size)
        }
    }
}

fn internal(err: rusqlite::Error) -> QueryError {
    QueryError::new(
        QueryErrorCode::Internal,
        format!("the query database could not be set up: {err}"),
        "This is a bug in Carrysheet, or the machine is out of memory.",
        "Try again with fewer or smaller sheets, and report the problem if it stays.",
    )
}

fn load_error(table: &str, err: rusqlite::Error) -> QueryError {
    let code = match &err {
        rusqlite::Error::SqliteFailure(e, _) => Some(e.code),
        _ => None,
    };
    let what = format!(
        "{} could not be loaded for a query: {err}",
        quote_ident(table)
    );
    match code {
        Some(ErrorCode::TooBig) => QueryError::new(
            QueryErrorCode::InvalidInput,
            what,
            "A cell or row is larger than SQLite accepts.",
            "Query a smaller range.",
        ),
        Some(ErrorCode::OutOfMemory) => QueryError::new(
            QueryErrorCode::TooLarge,
            what,
            "The sheets need more memory than the query budget allows.",
            "Query fewer sheets or a smaller range.",
        ),
        _ => QueryError::new(
            QueryErrorCode::Internal,
            what,
            "This is a bug in Carrysheet, or the machine is out of memory.",
            "Query a smaller range, and report the problem if it stays.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_sql() {
        assert!(is_blank_sql(""));
        assert!(is_blank_sql("  -- note\n /* x */ ;"));
        assert!(is_blank_sql("/* unterminated"));
        assert!(!is_blank_sql("-- a\nSELECT 1"));
        assert_eq!(first_keyword(" /* x */ select 1"), "SELECT");
        assert_eq!(first_keyword("(1)"), "");
    }

    #[test]
    fn blobs_show_as_hex() {
        let (json, _) = to_json(ValueRef::Blob(&[0xAB, 0x01]), false);
        assert_eq!(json, Json::String("x'AB01'".into()));
    }
}
