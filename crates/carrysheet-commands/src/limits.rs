//! Every limit on the work and memory of one call, in one place.
//!
//! A hostile or careless file or request must fail with an error that says
//! what, why and how to fix it, never hang or run out of memory (record
//! 0003). Issue 56 turns these into one shared work budget; until then each
//! is checked where the work happens: [`MAX_OPS`] and the fill limits when an
//! edit is parsed, the rest by the engine (the kernel hands them to GridCraft
//! for each call).

/// Most operations in one `edit` call.
pub const MAX_OPS: usize = 1_000;

/// Most cells one `set_value` or `set_formula` may fill.
pub const MAX_FILL_CELLS: u64 = 100_000;

/// Most cells all `set_value` and `set_formula` operations of one `edit`
/// call may fill together.
pub const MAX_BATCH_CELLS: u64 = 1_000_000;

/// Most cells one formula's array result may hold (two full columns).
pub const MAX_ARRAY_CELLS: u64 = 2 * 1_048_576;

/// Most units of calculation work in one call: about one per cell read
/// from a range, per formula evaluated and per cell of an array result.
pub const MAX_CALC_WORK: u64 = 2_000_000_000;

/// Most seconds one command may take, from opening the file to writing
/// it: reading, unpacking and parsing count, as does calculation. The
/// default for MCP and agent calls; the CLI may raise it with
/// `--max-seconds`, up to [`MAX_CLI_SECONDS`].
pub const MAX_CALL_SECONDS: f64 = 10.0;

/// The most `--max-seconds` accepts.
pub const MAX_CLI_SECONDS: f64 = 3_600.0;

/// Most cell formats, fonts, fills, borders or number formats in a file:
/// Excel's own limit on cell formats.
pub const MAX_STYLES: usize = 65_490;

/// Most shared strings in a file.
pub const MAX_SHARED_STRINGS: usize = 5_000_000;

/// Most elements and attributes in one XML part that is read whole.
pub const MAX_XML_NODES: usize = 2_000_000;

/// Most bytes one zip entry may unpack to (and most packed bytes read from one).
pub const MAX_ENTRY_BYTES: u64 = 512 << 20;

/// Unpacking must give at least one byte for every this many bytes read.
/// Real files give far more than they take; an entry of empty compressed
/// blocks gives nothing and costs seconds per megabyte.
pub const MIN_INFLATE_RATIO: u32 = 4;

/// Most characters of one cell's text shown by `read` and `overview`.
pub const MAX_CELL_TEXT_CHARS: usize = 2_000;

/// Most sheets listed by `overview`.
pub const MAX_LISTED_SHEETS: usize = 1_000;

/// Most characters of cell text in one `read` or `overview` answer.
pub const MAX_RESPONSE_TEXT_CHARS: usize = 200_000;

/// Most bytes of a csv file written.
pub const MAX_CSV_BYTES: usize = 512 << 20;
