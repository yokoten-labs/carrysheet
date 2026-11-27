//! Carrysheet as a library: bytes in, JSON out, on GridCraft (vendored in
//! `vendor/gridcraft`, record 0003); the program reads and writes the files
//! (record 0010, decision 2). [`Kernel`] runs every GridCraft call within
//! [`Limits`] and behind `catch_unwind`.

pub mod args;
pub mod commands;
pub mod edit;
pub mod error;
pub mod open;
pub mod range;
pub mod save;
pub mod text;
pub mod view;

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

pub use gridcraft_engine::core::limits::Limits;
use gridcraft_engine::core::limits::{self as gc, Exceeded};
use serde_json::{Map, Value};

use crate::args::Command;
use crate::commands::limits as values;
use crate::edit::{EditOp, EditResult};
use crate::error::{CommandError, ErrorCode};
use crate::open::{Book, MAX_FILE_BYTES};
use crate::save::{EncodeOptions, Encoded, FileFormat};
use crate::text::escape;

/// The commands [`run`] runs: those that work on a workbook.
pub const ENGINE_COMMANDS: &[&str] = &["overview", "read", "eval", "edit", "convert"];

/// Carrysheet's limits on one call; the values are in [`commands::limits`].
pub const LIMITS: Limits = Limits {
    array_cells: values::MAX_ARRAY_CELLS,
    work: values::MAX_CALC_WORK,
    seconds: Some(values::MAX_CALL_SECONDS),
    styles: values::MAX_STYLES,
    shared_strings: values::MAX_SHARED_STRINGS,
    xml_nodes: values::MAX_XML_NODES,
    entry_bytes: values::MAX_ENTRY_BYTES,
    min_inflate_ratio: values::MIN_INFLATE_RATIO,
};

/// A command's JSON fields, and for a save the bytes the caller writes.
#[derive(Debug)]
pub struct Answer {
    pub data: Map<String, Value>,
    pub save: Option<Vec<u8>>,
}

impl From<Map<String, Value>> for Answer {
    fn from(data: Map<String, Value>) -> Self {
        Answer { data, save: None }
    }
}

/// Runs one of [`ENGINE_COMMANDS`] on `input`, the bytes of the file `path`
/// names. `args` were checked by [`commands::validate_args`].
pub fn run(
    kernel: &Kernel,
    command: &Command,
    args: &Map<String, Value>,
    input: Vec<u8>,
) -> Result<Answer, CommandError> {
    run_within(kernel, command, args, input, None)
}

/// [`run`] with `seconds` as the time limit, one budget for open, work and save.
pub fn run_within(
    kernel: &Kernel,
    command: &Command,
    args: &Map<String, Value>,
    input: Vec<u8>,
    seconds: Option<f64>,
) -> Result<Answer, CommandError> {
    kernel.begin_call(seconds);
    let result = match command.name {
        "overview" => view::overview(kernel, args, input).map(Answer::from),
        "read" => view::read(kernel, args, input).map(Answer::from),
        "eval" => view::eval(kernel, args, input).map(Answer::from),
        "edit" => edit::edit(kernel, args, input),
        "convert" => save::convert(kernel, args, input),
        _ => Err(internal("running a command", "it is not an engine command")),
    };
    // A limit that ran out wins over what the calls returned.
    kernel.end_call()?;
    result
}

thread_local! {
    /// True between `begin_call` and `end_call`, whose one budget covers the command.
    static IN_COMMAND: Cell<bool> = const { Cell::new(false) };
}

/// GridCraft behind the limits and the panic guard: a caught panic is an `internal_error`, never
/// a success (record 0003, point 1; wasm32 aborts); out of work or time is `limit_exceeded`.
#[derive(Debug, Clone, Copy)]
pub struct Kernel {
    limits: Limits,
}

impl Default for Kernel {
    fn default() -> Self {
        Kernel { limits: LIMITS }
    }
}

impl Kernel {
    pub fn new() -> Kernel {
        Kernel::default()
    }

    /// The same kernel with other limits (tests use lower ones).
    pub fn with_limits(self, limits: Limits) -> Kernel {
        Kernel { limits }
    }

    /// Starts one command's budget for every call until [`Kernel::end_call`].
    /// `seconds` replaces the time limit (the CLI's `--max-seconds`).
    pub fn begin_call(&self, seconds: Option<f64>) {
        let mut limits = self.limits;
        if seconds.is_some() {
            limits.seconds = seconds;
        }
        gc::set(limits);
        IN_COMMAND.with(|c| c.set(true));
    }

    /// Ends the budget: an error when a limit ran out during the command,
    /// whatever the calls returned.
    pub fn end_call(&self) -> Result<(), CommandError> {
        IN_COMMAND.with(|c| c.set(false));
        check_limits("the command", true)
    }

    /// Opens and recalculates a file's bytes. `path` names it in messages
    /// and picks the format when the bytes do not say.
    pub fn open_bytes(&self, path: &Path, bytes: Vec<u8>) -> Result<Book, CommandError> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(open::too_large(path));
        }
        self.guard("opening the file", || Book::from_bytes(path, bytes))
    }

    /// Applies operations in order, all or nothing, with no undo history kept.
    pub fn edit(&self, book: &mut Book, ops: &[EditOp]) -> Result<EditResult, CommandError> {
        self.guard("editing", || edit::apply(book, ops))
    }

    /// Encodes the book for `path` in `format`, and says what the saved
    /// file would lose against the opened one. Nothing is written.
    pub fn encode(
        &self,
        book: &Book,
        path: &Path,
        format: FileFormat,
        options: &EncodeOptions<'_>,
    ) -> Result<Encoded, CommandError> {
        self.guard("saving", || {
            save::encode(book, path, format, options, values::MAX_CSV_BYTES)
        })
    }

    /// Runs `f` within the limits; outside a command, each call gets its own.
    pub(crate) fn guard<T>(
        &self,
        doing: &str,
        f: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let in_command = IN_COMMAND.with(Cell::get);
        if !in_command {
            gc::set(self.limits);
        }
        let result = guard(doing, f);
        check_limits(doing, !in_command)?;
        result
    }
}

/// The error of a limit that ran out while `doing`: the call's result, even
/// a success, is not to be trusted then. `reset` lifts GridCraft's limits.
fn check_limits(doing: &str, reset: bool) -> Result<(), CommandError> {
    let (exceeded, limits) = (gc::exceeded(), gc::get());
    if reset {
        gc::set(Limits::UNLIMITED);
    }
    match exceeded {
        None => Ok(()),
        Some(Exceeded::Work) => Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("{doing} needs more calculation than Carrysheet allows in one call"),
            format!(
                "it stopped after {} units of work (about one per cell read and per formula computed); formulas that read long ranges, filled down many rows, cost rows times rows",
                limits.work
            ),
            "Make the formulas read smaller ranges, or work on fewer cells per call; nothing was written.",
        )),
        Some(Exceeded::Time) => Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("{doing} took longer than Carrysheet allows in one call"),
            format!(
                "the command was stopped after {} seconds (opening, calculating and saving count)",
                limits.seconds.unwrap_or_default()
            ),
            "Work on a smaller file or fewer cells per call; on the command line, --max-seconds allows more time. Nothing was written.",
        )),
    }
}

/// The version shown in messages.
pub const GRIDCRAFT_VERSION: &str = "GridCraft 0.8.0 (823c565)";

/// Runs `f`, turning a panic into an `internal_error`.
pub(crate) fn guard<T>(
    doing: &str,
    f: impl FnOnce() -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|payload| {
        let text = payload.downcast_ref::<&str>().map(|s| (*s).to_owned());
        let text = text.or_else(|| payload.downcast_ref::<String>().cloned());
        Err(internal(doing, text.as_deref().unwrap_or("no message")))
    })
}

/// A bug in the kernel. The fix asks for the file: only it shows the cause.
pub(crate) fn internal(doing: &str, detail: &str) -> CommandError {
    CommandError::new(
        ErrorCode::Internal,
        format!("Carrysheet's kernel failed while {doing}; nothing was written"),
        format!(
            "a bug in the calculation kernel ({GRIDCRAFT_VERSION}) stopped the work: {}",
            escape(detail, 160)
        ),
        "Please report it at https://github.com/yokoten-labs/carrysheet/issues, with the file if you can share it. Do not trust other results for this file until then.",
    )
}
