//! Errors for people and agents: a code, what happened, why, and how to fix it.

use std::fmt;

use crate::range::RefError;
use serde::Serialize;

/// How a command ended, as a process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExitCode {
    /// 0: the command did what was asked.
    Done = 0,
    /// 1: the command failed.
    Failed = 1,
    /// 2: the command was called wrongly (unknown flag, bad argument).
    Usage = 2,
    /// 3: there was nothing to do (for example, already installed).
    NothingToDo = 3,
}

impl ExitCode {
    /// The number the process exits with.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// A stable, machine-readable error code. Agents may match on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The command line could not be parsed (unknown flag, missing argument).
    Usage,
    /// An argument has the wrong type or value.
    InvalidArgument,
    /// A cell or range reference is not valid A1 text.
    InvalidReference,
    /// The work was already done; nothing changed.
    NothingToDo,
    /// A file or folder does not exist.
    FileNotFound,
    /// The output file exists and overwriting was not confirmed.
    FileExists,
    /// The operating system refused access.
    PermissionDenied,
    /// The path is outside the folder this program may use (`--root`).
    OutsideRoot,
    /// The file format is not one Carrysheet reads or writes.
    UnsupportedFormat,
    /// The file is damaged or not what its name says; treated as hostile.
    DamagedFile,
    /// The file is larger, or holds more, than Carrysheet accepts.
    LimitExceeded,
    /// The workbook has no sheet with that name.
    NoSuchSheet,
    /// Deleting or overwriting needs `confirm: true`, and it was not given.
    ConfirmationRequired,
    /// A formula could not be computed.
    Formula,
    /// Reading or writing failed.
    Io,
    /// A bug in Carrysheet (including a caught panic in the kernel).
    #[serde(rename = "internal_error")]
    Internal,
}

impl ErrorCode {
    /// The code as written in JSON, such as `invalid_argument`.
    pub fn as_str(self) -> String {
        let name = serde_json::to_value(self).unwrap_or_default();
        name.as_str().unwrap_or_default().to_owned()
    }

    /// The exit code this error ends the process with.
    pub fn exit_code(self) -> ExitCode {
        match self {
            ErrorCode::Usage | ErrorCode::InvalidArgument | ErrorCode::InvalidReference => {
                ExitCode::Usage
            }
            ErrorCode::NothingToDo => ExitCode::NothingToDo,
            _ => ExitCode::Failed,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// An error with the three parts people and agents need, plus a code. A
/// short `fix` comes right after `what`, because some clients keep only the
/// start of an error. The fields are the JSON keys, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandError {
    pub code: ErrorCode,
    /// What happened.
    pub what: String,
    /// How to fix it, ideally with an example.
    pub fix: String,
    /// Why it happened.
    pub why: String,
}

impl CommandError {
    pub fn new(
        code: ErrorCode,
        what: impl Into<String>,
        why: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        CommandError { code, what: what.into(), why: why.into(), fix: fix.into() }
    }

    /// An argument with a wrong value.
    pub fn invalid_argument(name: &str, why: impl Into<String>, fix: impl Into<String>) -> Self {
        let what = format!("the argument `{name}` is not valid");
        CommandError::new(ErrorCode::InvalidArgument, what, why, fix)
    }

    /// The exit code for this error.
    pub fn exit_code(&self) -> ExitCode {
        self.code.exit_code()
    }

    /// `{"code","what","fix","why"}` as JSON, the fix before the reason.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    /// The full error envelope: `{"ok": false, "error": {...}}`.
    pub fn to_envelope(&self) -> serde_json::Value {
        serde_json::json!({ "ok": false, "error": self.to_json() })
    }

    /// Plain text for a terminal: three lines, no colour, the fix second.
    pub fn to_text(&self) -> String {
        format!("error: {}\n  fix: {}\n  why: {}", self.what, self.fix, self.why)
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.what)
    }
}

impl std::error::Error for CommandError {}

impl From<RefError> for CommandError {
    fn from(e: RefError) -> Self {
        CommandError::new(
            ErrorCode::InvalidReference,
            e.to_string(),
            "references are written in A1 style, with an optional sheet name before '!'",
            "Write a cell or range like B7, A1:C10, Sheet1!A1:C10 or 'My sheet'!A:A.",
        )
    }
}
