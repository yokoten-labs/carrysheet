//! Carrysheet's calculation kernel: the command layer's [`Engine`] on
//! GridCraft, vendored in `vendor/gridcraft` (record 0003).
//!
//! - [`Kernel::open`] reads a file (xlsx, xlsm, xls, xlsb, csv) and
//!   recalculates it, so every value read is computed, not cached.
//! - Edits go through GridCraft's command layer, which recalculates only
//!   what depends on the changed cells. One-shot calls keep no undo history.
//! - [`Kernel::encode`] writes xlsx or csv and compares the result with the
//!   opened file, so a save that would drop something says so first
//!   (record 0003, point 3).
//! - Every call into GridCraft runs behind `catch_unwind`. A caught panic is
//!   an `internal_error`, never a success (record 0003, point 1). On wasm32
//!   a panic aborts instead.
//! - Every call runs within [`Limits`] on work, time and memory; one that
//!   runs out is a `limit_exceeded` error, never a hang or an abort.

mod book;
mod edit;
mod limits;
mod package;
mod save;

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use carrysheet_commands::engine::{
    EditOp, EditResult, EncodeOptions, Encoded, Engine, Evaluation, FileFormat, Overview, ReadPage,
    ReadRequest, save::clean_from_file,
};
use carrysheet_commands::{CommandError, ErrorCode};

pub use book::Book;
pub use limits::Limits;

/// The largest file opened. 32-bit programs get less (record 0003, point 4).
pub const MAX_FILE_BYTES: u64 = if cfg!(target_pointer_width = "64") {
    1 << 30
} else {
    256 << 20
};

/// The GridCraft engine.
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

    /// The limits each call runs within.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Opens a file's bytes. `path` names it in messages and picks the
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

impl Engine for Kernel {
    type Book = Book;

    fn begin_call(&self, seconds: Option<f64>) {
        self.limits.begin_call(seconds);
    }

    fn end_call(&self) -> Result<(), CommandError> {
        self.limits.end_call()
    }

    fn name(&self) -> &'static str {
        "gridcraft"
    }

    fn open(&self, path: &Path) -> Result<Book, CommandError> {
        let bytes = book::read_file(path)?;
        self.guard("opening the file", || Book::from_bytes(path, bytes))
    }

    fn overview(&self, book: &Book) -> Result<Overview, CommandError> {
        self.guard("reading the workbook", || book.overview())
    }

    fn read_range(&self, book: &Book, request: &ReadRequest) -> Result<ReadPage, CommandError> {
        self.guard("reading cells", || book.read(request))
    }

    fn evaluate(
        &self,
        book: &Book,
        sheet: Option<&str>,
        formula: &str,
    ) -> Result<Evaluation, CommandError> {
        self.guard("computing the formula", || book.evaluate(sheet, formula))
    }

    fn save(&self, book: &Book, path: &Path, format: FileFormat) -> Result<(), CommandError> {
        let encoded = self.encode(book, path, format, None)?;
        carrysheet_commands::engine::save::write_atomic(path, &encoded.bytes)
    }

    fn edit(&self, book: &mut Book, ops: &[EditOp]) -> Result<EditResult, CommandError> {
        self.guard("editing", || edit::apply(book, ops))
    }

    fn encode(
        &self,
        book: &Book,
        path: &Path,
        format: FileFormat,
        sheet: Option<&str>,
    ) -> Result<Encoded, CommandError> {
        let options = EncodeOptions {
            sheet,
            ..EncodeOptions::default()
        };
        self.encode_with(book, path, format, &options)
    }

    fn encode_with(
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
}

/// Runs `f`, turning a panic into an `internal_error`.
fn guard<T>(doing: &str, f: impl FnOnce() -> Result<T, CommandError>) -> Result<T, CommandError> {
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
            clean_from_file(detail, 160)
        ),
        "Please report it: run `carrysheet-cli feedback file \"<what you did>\"` and attach the file (sanitize it first). Do not trust other results for this file until then.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panics_become_internal_errors_not_success() {
        let r: Result<(), CommandError> = guard("testing", || panic!("boom at A1"));
        let e = r.unwrap_err();
        assert_eq!(e.code, ErrorCode::Internal);
        assert_eq!(e.code.as_str(), "internal_error");
        assert!(e.why.contains("boom at A1"), "{}", e.why);
        assert!(e.fix.contains("feedback"), "{}", e.fix);
        let r: Result<u8, CommandError> = guard("testing", || panic!("{}", String::from("x")));
        assert!(r.unwrap_err().why.contains('x'));
    }
}
