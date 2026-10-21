//! Errors for people and agents: a code, what happened, why, and how to fix it.

use std::fmt;

use carrysheet_model::RefError;
use serde::Serialize;

use crate::paths::redact_home;

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
    /// The command exists but is not built yet.
    NotImplemented,
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
    /// The workbook has no sheet with that name.
    NoSuchSheet,
    /// The file is damaged, or its structure is not what its format says.
    DamagedFile,
    /// The file is over a safety limit on sizes or counts (for example a
    /// zip bomb); it is refused rather than read.
    LimitExceeded,
    /// A confirmation was needed and the answer was no (or nobody could answer).
    Declined,
    /// Deleting or overwriting needs `confirm: true`, and it was not given.
    ConfirmationRequired,
    /// A formula could not be computed.
    Formula,
    /// Reading or writing failed.
    Io,
    /// A bug in Carrysheet.
    Internal,
}

impl ErrorCode {
    /// The code as written in JSON, such as `not_implemented`.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "usage",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::InvalidReference => "invalid_reference",
            ErrorCode::NotImplemented => "not_implemented",
            ErrorCode::NothingToDo => "nothing_to_do",
            ErrorCode::FileNotFound => "file_not_found",
            ErrorCode::FileExists => "file_exists",
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::OutsideRoot => "outside_root",
            ErrorCode::UnsupportedFormat => "unsupported_format",
            ErrorCode::NoSuchSheet => "no_such_sheet",
            ErrorCode::DamagedFile => "damaged_file",
            ErrorCode::LimitExceeded => "limit_exceeded",
            ErrorCode::Declined => "declined",
            ErrorCode::ConfirmationRequired => "confirmation_required",
            ErrorCode::Formula => "formula",
            ErrorCode::Io => "io",
            ErrorCode::Internal => "internal",
        }
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
        f.write_str(self.as_str())
    }
}

/// An error with the three parts people and agents need, plus a code.
///
/// Keep `fix` short. It is shown right after `what`, before `why`, because
/// some clients keep only the start of an error.
///
/// The parts often repeat what the user typed (a path, a reference), so
/// every way of showing an error (`to_json`, `to_text`, `Display` and
/// `Serialize`) replaces the home folder with `~`. The fields themselves
/// keep the text as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        CommandError {
            code,
            what: what.into(),
            why: why.into(),
            fix: fix.into(),
        }
    }

    /// A command that is in the table but not built yet. `cli_name` is how it
    /// is typed, such as `mcp install`.
    pub fn not_implemented(cli_name: &str) -> Self {
        CommandError::new(
            ErrorCode::NotImplemented,
            format!("`{cli_name}` is not implemented yet"),
            "Carrysheet is in early development; this command is planned for the first release but not built.",
            "Run `carrysheet-cli commands` to see which commands work today.",
        )
    }

    /// An argument with a wrong value.
    pub fn invalid_argument(name: &str, why: impl Into<String>, fix: impl Into<String>) -> Self {
        CommandError::new(
            ErrorCode::InvalidArgument,
            format!("the argument `{name}` is not valid"),
            why,
            fix,
        )
    }

    /// The exit code for this error.
    pub fn exit_code(&self) -> ExitCode {
        self.code.exit_code()
    }

    /// The same error with the home folder shown as `~` in every part.
    pub fn redacted(&self) -> CommandError {
        CommandError {
            code: self.code,
            what: redact_home(&self.what),
            fix: redact_home(&self.fix),
            why: redact_home(&self.why),
        }
    }

    /// `{"code","what","fix","why"}` as JSON, the fix before the reason.
    pub fn to_json(&self) -> serde_json::Value {
        let shown = self.redacted();
        serde_json::json!({
            "code": shown.code.as_str(),
            "what": shown.what,
            "fix": shown.fix,
            "why": shown.why,
        })
    }

    /// The full error envelope: `{"ok": false, "error": {...}}`.
    pub fn to_envelope(&self) -> serde_json::Value {
        serde_json::json!({ "ok": false, "error": self.to_json() })
    }

    /// Plain text for a terminal: three lines, no colour, the fix second.
    pub fn to_text(&self) -> String {
        let shown = self.redacted();
        format!(
            "error: {}\n  fix: {}\n  why: {}",
            shown.what, shown.fix, shown.why
        )
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact_home(&self.what))
    }
}

impl std::error::Error for CommandError {}

impl Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes() {
        assert_eq!(ExitCode::Done.code(), 0);
        assert_eq!(ErrorCode::Io.exit_code().code(), 1);
        assert_eq!(ErrorCode::NotImplemented.exit_code(), ExitCode::Failed);
        assert_eq!(ErrorCode::Usage.exit_code().code(), 2);
        assert_eq!(ErrorCode::InvalidArgument.exit_code(), ExitCode::Usage);
        assert_eq!(ErrorCode::InvalidReference.exit_code(), ExitCode::Usage);
        assert_eq!(ErrorCode::NothingToDo.exit_code().code(), 3);
        assert_eq!(ErrorCode::Declined.exit_code(), ExitCode::Failed);
        assert_eq!(ErrorCode::ConfirmationRequired.exit_code().code(), 1);
        assert_eq!(
            serde_json::to_value(ErrorCode::ConfirmationRequired).unwrap(),
            ErrorCode::ConfirmationRequired.as_str()
        );
    }

    #[test]
    fn json_shape() {
        let e = CommandError::not_implemented("mcp install");
        let v = e.to_envelope();
        assert_eq!(v["ok"], false);
        let err = v["error"].as_object().unwrap();
        let keys: Vec<_> = err.keys().map(String::as_str).collect();
        assert_eq!(keys, ["code", "what", "fix", "why"]);
        assert_eq!(err["code"], "not_implemented");
        assert!(err["what"].as_str().unwrap().contains("mcp install"));
        // The serde form matches the hand-written one.
        assert_eq!(serde_json::to_value(&e).unwrap(), e.to_json());
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            e.to_json().to_string(),
            "same key order"
        );
    }

    #[test]
    fn codes_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_value(ErrorCode::NotImplemented).unwrap(),
            "not_implemented"
        );
        assert_eq!(ErrorCode::FileNotFound.to_string(), "file_not_found");
        assert_eq!(ErrorCode::DamagedFile.to_string(), "damaged_file");
        assert_eq!(ErrorCode::LimitExceeded.to_string(), "limit_exceeded");
        assert_eq!(ErrorCode::LimitExceeded.exit_code(), ExitCode::Failed);
    }

    #[test]
    fn text_has_three_parts() {
        let text = CommandError::new(ErrorCode::Io, "a", "b", "c").to_text();
        assert_eq!(text, "error: a\n  fix: c\n  why: b");
    }

    #[test]
    fn every_way_of_showing_hides_the_home_folder() {
        let Some(home) = crate::paths::home_dir() else {
            return;
        };
        let p = home.join("q1.xlsx").display().to_string();
        let e = CommandError::new(ErrorCode::Io, format!("cannot open {p}"), &p, &p);
        let home = home.display().to_string();
        for shown in [
            e.to_text(),
            e.to_json().to_string(),
            e.to_envelope().to_string(),
            serde_json::to_string(&e).unwrap(),
            e.to_string(),
        ] {
            assert!(!shown.contains(&home), "{shown}");
            assert!(shown.contains('~'), "{shown}");
        }
        // A reference error repeats what was typed.
        let typed = format!("'{p}'!A1");
        let e: CommandError = carrysheet_model::SheetRange::parse(&typed)
            .unwrap_err()
            .into();
        assert!(e.what.contains(&home), "the field keeps the text");
        assert!(!e.to_json().to_string().contains(&home));
    }

    #[test]
    fn ref_errors_become_usage_errors() {
        let e: CommandError = carrysheet_model::CellRef::parse("XFE1").unwrap_err().into();
        assert_eq!(e.code, ErrorCode::InvalidReference);
        assert_eq!(e.exit_code(), ExitCode::Usage);
        assert!(e.fix.contains("A1:C10"));
    }
}
