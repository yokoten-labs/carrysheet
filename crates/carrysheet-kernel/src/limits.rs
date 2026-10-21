//! The limits of one call into GridCraft. The values come from
//! [`carrysheet_commands::limits`]; GridCraft reads them from
//! `gridcraft_core::limits` while the call runs (vendor README, patches).

use std::cell::Cell;

use carrysheet_commands::limits as values;
use carrysheet_commands::{CommandError, ErrorCode};
use gridcraft_engine::core::limits::{self as gc, Exceeded};

/// What one call may cost. [`Limits::default`] is Carrysheet's; tests may
/// lower them with [`crate::Kernel::with_limits`].
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
    pub(crate) fn begin_call(&self, seconds: Option<f64>) {
        let mut limits = *self;
        if let Some(s) = seconds {
            limits.calc_seconds = s;
        }
        limits.arm();
        IN_CALL.with(|c| c.set(Some(limits)));
    }

    /// Ends the command's budget: the error of a limit that ran out.
    pub(crate) fn end_call(&self) -> Result<(), CommandError> {
        let limits = IN_CALL.with(Cell::take).unwrap_or(*self);
        limits.disarm("the command")
    }

    /// Before one call: arms the limits unless a command's budget runs.
    pub(crate) fn enter(&self) {
        if IN_CALL.with(Cell::get).is_none() {
            self.arm();
        }
    }

    /// After one call: the error of a limit that ran out. Outside a
    /// command's budget the limits are reset.
    pub(crate) fn leave(&self, doing: &str) -> Result<(), CommandError> {
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
    pub(crate) fn arm(&self) {
        gc::set(self.gridcraft());
    }

    /// After a call: an error when a limit ran out (the call's result, even
    /// a success, is not to be trusted then). Resets GridCraft's limits.
    pub(crate) fn disarm(&self, doing: &str) -> Result<(), CommandError> {
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
