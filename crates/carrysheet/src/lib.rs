//! Carrysheet as a library: bytes in, JSON out, on GridCraft (vendored in
//! `vendor/gridcraft`, record 0003).
//!
//! - This file: [`run`] runs one command; [`Kernel`] puts every call into
//!   GridCraft within [`Limits`] and behind `catch_unwind`.
//! - [`open`]: a file's bytes to a recalculated workbook; the csv front end.
//! - [`view`]: `overview`, `read` and `eval`.
//! - [`edit`]: the `edit` command.
//! - [`save`]: `convert`, and what a save keeps and loses.
//! - [`range`]: A1 ranges with a sheet name.
//! - [`text`]: text from a file, made safe to show.
//! - [`error`]: errors with a code, what, why and fix, and exit codes.
//! - [`args`]: one struct per command: its flags, schema and checks.
//! - [`commands`]: the names a caller needs, and the limit values.
//!
//! The program reads and writes the files (record 0010, decision 2).

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

use std::any::Any;
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use gridcraft_engine::core::limits::{self as gc, Exceeded};
use serde_json::{Map, Value};

use crate::args::Command;
use crate::commands::limits as values;
use crate::edit::{EditOp, EditResult};
use crate::error::{CommandError, ErrorCode};
use crate::open::{Book, MAX_FILE_BYTES};
use crate::save::{EncodeOptions, Encoded, FileFormat};
use crate::text::escape;
use crate::view::{Evaluation, Overview, ReadPage, ReadRequest};

/// The commands [`run`] runs: those that work on a workbook.
pub const ENGINE_COMMANDS: &[&str] = &["overview", "read", "eval", "edit", "convert"];

/// Runs one of [`ENGINE_COMMANDS`]. `args` were checked by
/// [`validate_args`](crate::commands::validate_args) (absent ones are
/// `null`); `input` holds the bytes of the file `path` names.
pub fn run(
    kernel: &Kernel,
    command: &Command,
    args: &Map<String, Value>,
    input: Vec<u8>,
) -> Result<Answer, CommandError> {
    run_within(kernel, command, args, input, None)
}

/// What a command gives back: the JSON fields of its result, and for a
/// save, the bytes the caller writes to `output`. The caller then adds
/// `written` and `bytes`.
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

/// [`run`] with the command's time limit in seconds (`None`: the kernel's,
/// [`crate::commands::limits::MAX_CALL_SECONDS`]). One budget covers the
/// whole command: opening, the work and the save.
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
        _ => Err(CommandError::not_implemented(&command.cli_name())),
    };
    // A limit that ran out wins over what the calls returned.
    kernel.end_call()?;
    result
}

/// GridCraft, with every call inside the limits and the panic guard.
///
/// - A caught panic is an `internal_error`, never a success (record 0003,
///   point 1). On wasm32 a panic aborts instead.
/// - A call that runs out of work, time or memory is a `limit_exceeded`
///   error, never a hang or an abort.
#[derive(Debug, Default, Clone, Copy)]
pub struct Kernel {
    limits: Limits,
}

impl Kernel {
    pub fn new() -> Kernel {
        Kernel::default()
    }

    /// The same kernel with other limits (tests use lower ones).
    pub fn with_limits(self, limits: Limits) -> Kernel {
        Kernel { limits }
    }

    /// Starts one command's budget: the work and time limits cover every
    /// call that follows (open, work, save) until [`Kernel::end_call`].
    /// `seconds` replaces the time limit (the CLI's `--max-seconds`).
    pub fn begin_call(&self, seconds: Option<f64>) {
        self.limits.begin_call(seconds);
    }

    /// Ends the budget [`Kernel::begin_call`] started: an error when a
    /// limit ran out during the command, whatever the calls returned.
    pub fn end_call(&self) -> Result<(), CommandError> {
        self.limits.end_call()
    }

    /// Opens a file's bytes (xlsx, xlsm, xls, xlsb, ods, csv) and
    /// recalculates them. `path` names it in messages and picks the
    /// format when the bytes do not say.
    pub fn open_bytes(&self, path: &Path, bytes: Vec<u8>) -> Result<Book, CommandError> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(CommandError::new(
                ErrorCode::LimitExceeded,
                "the file is too large to open",
                format!("Carrysheet opens files up to {} MB", MAX_FILE_BYTES >> 20),
                "Split the workbook, or save a copy with fewer sheets or rows.",
            ));
        }
        self.guard("opening the file", || Book::from_bytes(path, bytes))
    }

    /// Sheets, used ranges and counts.
    pub fn overview(&self, book: &Book) -> Result<Overview, CommandError> {
        self.guard("reading the workbook", || book.overview())
    }

    /// One page of a range.
    pub fn read_range(&self, book: &Book, request: &ReadRequest) -> Result<ReadPage, CommandError> {
        self.guard("reading cells", || book.read(request))
    }

    /// Computes a formula (en-US, with or without `=`) without changing the
    /// book. `sheet` is where unqualified references point; `None` means the
    /// first sheet.
    pub fn evaluate(
        &self,
        book: &Book,
        sheet: Option<&str>,
        formula: &str,
    ) -> Result<Evaluation, CommandError> {
        self.guard("computing the formula", || book.evaluate(sheet, formula))
    }

    /// Applies operations in order, all or nothing: on an error the book is
    /// left as it was. One-shot calls keep no undo history.
    pub fn edit(&self, book: &mut Book, ops: &[EditOp]) -> Result<EditResult, CommandError> {
        self.guard("editing", || edit::apply(book, ops))
    }

    /// Encodes the book for `path` in `format`, and says what the saved
    /// file would lose compared with the file that was opened, so a save
    /// that would drop something says so first. Nothing is written.
    pub fn encode(
        &self,
        book: &Book,
        path: &Path,
        format: FileFormat,
        options: &EncodeOptions<'_>,
    ) -> Result<Encoded, CommandError> {
        let csv_bytes = self.limits.csv_bytes;
        self.guard("saving", || {
            save::encode(book, path, format, options, csv_bytes)
        })
    }

    /// Runs `f` within the limits, turning a panic into an `internal_error`
    /// and a limit that ran out into `limit_exceeded`.
    fn guard<T>(
        &self,
        doing: &str,
        f: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        self.limits.enter();
        let result = guard(doing, f);
        self.limits.leave(doing)?;
        result
    }
}

/// The version shown in messages.
pub const GRIDCRAFT_VERSION: &str = "GridCraft 0.8.0 (823c565)";

/// Runs `f`, turning a panic into an `internal_error`.
pub(crate) fn guard<T>(
    doing: &str,
    f: impl FnOnce() -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => Err(internal(doing, &panic_text(payload.as_ref()))),
    }
}

fn panic_text(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_owned())
}

/// A bug in the kernel. The fix asks for the file, because only the file
/// shows what went wrong.
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

/// The limits of one call into GridCraft. The values come from
/// [`crate::commands::limits`]; GridCraft reads them from
/// `gridcraft_core::limits` while the call runs (vendor README, patches).
///
/// What one call may cost. [`Limits::default`] is Carrysheet's; tests may
/// lower them with [`Kernel::with_limits`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Most cells one formula's array result may hold.
    pub array_cells: u64,
    /// Most units of calculation work.
    pub calc_work: u64,
    /// Most seconds one command takes, opening and saving included.
    pub calc_seconds: f64,
    /// Most entries in each list of the styles part.
    pub styles: usize,
    /// Most shared strings.
    pub shared_strings: usize,
    /// Most XML elements and attributes in one part read whole.
    pub xml_nodes: usize,
    /// Most bytes one zip entry may unpack to or read.
    pub entry_bytes: u64,
    /// Unpacking must give one byte for at most this many read.
    pub min_inflate_ratio: u32,
    /// Most bytes of a csv file written.
    pub csv_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            array_cells: values::MAX_ARRAY_CELLS,
            calc_work: values::MAX_CALC_WORK,
            calc_seconds: values::MAX_CALL_SECONDS,
            styles: values::MAX_STYLES,
            shared_strings: values::MAX_SHARED_STRINGS,
            xml_nodes: values::MAX_XML_NODES,
            entry_bytes: values::MAX_ENTRY_BYTES,
            min_inflate_ratio: values::MIN_INFLATE_RATIO,
            csv_bytes: values::MAX_CSV_BYTES,
        }
    }
}

thread_local! {
    /// Set between `begin_call` and `end_call`: one budget for the whole
    /// command, so calls in between neither reset nor clear it.
    static IN_CALL: Cell<Option<Limits>> = const { Cell::new(None) };
}

impl Limits {
    /// Starts one command's budget on this thread (round 2, N5).
    fn begin_call(&self, seconds: Option<f64>) {
        let mut limits = *self;
        if let Some(s) = seconds {
            limits.calc_seconds = s;
        }
        limits.arm();
        IN_CALL.with(|c| c.set(Some(limits)));
    }

    /// Ends the command's budget: the error of a limit that ran out.
    fn end_call(&self) -> Result<(), CommandError> {
        let limits = IN_CALL.with(Cell::take).unwrap_or(*self);
        limits.disarm("the command")
    }

    /// Before one call: arms the limits unless a command's budget runs.
    fn enter(&self) {
        if IN_CALL.with(Cell::get).is_none() {
            self.arm();
        }
    }

    /// After one call: the error of a limit that ran out. Outside a
    /// command's budget the limits are reset.
    fn leave(&self, doing: &str) -> Result<(), CommandError> {
        if let Some(call) = IN_CALL.with(Cell::get) {
            return match gc::exceeded() {
                None => Ok(()),
                Some(e) => Err(call.error(e, doing)),
            };
        }
        self.disarm(doing)
    }

    fn gridcraft(&self) -> gc::Limits {
        gc::Limits {
            array_cells: self.array_cells,
            work: self.calc_work,
            seconds: Some(self.calc_seconds),
            styles: self.styles,
            shared_strings: self.shared_strings,
            xml_nodes: self.xml_nodes,
            entry_bytes: self.entry_bytes,
            min_inflate_ratio: self.min_inflate_ratio,
        }
    }

    /// Sets these limits for GridCraft on this thread and starts counting.
    fn arm(&self) {
        gc::set(self.gridcraft());
    }

    /// After a call: an error when a limit ran out (the call's result, even
    /// a success, is not to be trusted then). Resets GridCraft's limits.
    fn disarm(&self, doing: &str) -> Result<(), CommandError> {
        let exceeded = gc::exceeded();
        gc::set(gc::Limits::UNLIMITED);
        match exceeded {
            None => Ok(()),
            Some(e) => Err(self.error(e, doing)),
        }
    }

    fn error(&self, exceeded: Exceeded, doing: &str) -> CommandError {
        match exceeded {
            Exceeded::Work => CommandError::new(
                ErrorCode::LimitExceeded,
                format!("{doing} needs more calculation than Carrysheet allows in one call"),
                format!(
                    "it stopped after {} units of work (about one per cell read and per formula computed); formulas that read long ranges, filled down many rows, cost rows times rows",
                    self.calc_work
                ),
                "Make the formulas read smaller ranges, or work on fewer cells per call; nothing was written.",
            ),
            Exceeded::Time => CommandError::new(
                ErrorCode::LimitExceeded,
                format!("{doing} took longer than Carrysheet allows in one call"),
                format!(
                    "the command was stopped after {} seconds (opening, calculating and saving count)",
                    self.calc_seconds
                ),
                "Work on a smaller file or fewer cells per call; on the command line, --max-seconds allows more time. Nothing was written.",
            ),
        }
    }
}
