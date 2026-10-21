//! Query errors: a code, what happened, why, and how to fix it.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A stable, machine-readable error code. Agents may match on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryErrorCode {
    /// The tables given to the engine could not be loaded (too wide, too big).
    InvalidInput,
    /// The query text is empty or only a comment.
    EmptyQuery,
    /// The query text holds more than one statement.
    MultipleStatements,
    /// The SQL could not be parsed.
    Syntax,
    /// The query names a table that does not exist.
    NoSuchTable,
    /// The query names a column that does not exist.
    NoSuchColumn,
    /// The query tries to write, attach a file, change a setting or load code.
    NotAllowed,
    /// The query ran more steps than allowed (for example a huge cross join).
    StepLimit,
    /// The query ran longer than allowed.
    TimeLimit,
    /// A value, the query text or the result is larger than allowed.
    TooLarge,
    /// Any other error SQLite reports.
    Sql,
    /// A bug in Carrysheet.
    Internal,
}

impl QueryErrorCode {
    /// The code as written in JSON, such as `no_such_table`.
    pub fn as_str(self) -> &'static str {
        match self {
            QueryErrorCode::InvalidInput => "invalid_input",
            QueryErrorCode::EmptyQuery => "empty_query",
            QueryErrorCode::MultipleStatements => "multiple_statements",
            QueryErrorCode::Syntax => "syntax",
            QueryErrorCode::NoSuchTable => "no_such_table",
            QueryErrorCode::NoSuchColumn => "no_such_column",
            QueryErrorCode::NotAllowed => "not_allowed",
            QueryErrorCode::StepLimit => "step_limit",
            QueryErrorCode::TimeLimit => "time_limit",
            QueryErrorCode::TooLarge => "too_large",
            QueryErrorCode::Sql => "sql",
            QueryErrorCode::Internal => "internal",
        }
    }
}

impl fmt::Display for QueryErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error with the three parts people and agents need, plus a code.
/// Serialized with the fix before the reason, because some clients keep only
/// the start of an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryError {
    pub code: QueryErrorCode,
    /// What happened.
    pub what: String,
    /// How to fix it, ideally with an example.
    pub fix: String,
    /// Why it happened.
    pub why: String,
}

impl QueryError {
    pub fn new(
        code: QueryErrorCode,
        what: impl Into<String>,
        why: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        QueryError {
            code,
            what: what.into(),
            why: why.into(),
            fix: fix.into(),
        }
    }
}

impl fmt::Display for QueryError {
    /// Three lines, the fix second: `what`, `  fix: ...`, `  why: ...`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n  fix: {}\n  why: {}", self.what, self.fix, self.why)
    }
}

impl std::error::Error for QueryError {}
