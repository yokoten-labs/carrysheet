//! Limits on the work and memory of one call, set by the program that embeds GridCraft
//! (Carrysheet patch, see the vendor README).
//!
//! The limits and the work counter live in thread-local storage: a call runs on one thread, and a
//! caller sets its limits with [`set`] before the call and reads [`exceeded`] after it. Without
//! [`set`] the defaults keep upstream's behaviour (no work or time limit).
//!
//! Work is counted in rough units: one per cell read from a range, per formula evaluated and per
//! cell of an array result. When the work or the time runs out, [`charge`] returns `false` from
//! then on, the calculation gives `#CALC!` quickly, and [`exceeded`] says which limit ran out.

use std::cell::{Cell, RefCell};

/// The limits of one call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// Most cells one formula's array result may hold.
    pub array_cells: u64,
    /// Most units of calculation work.
    pub work: u64,
    /// Most seconds of calculation, checked as work is counted. `None`: no time limit.
    pub seconds: Option<f64>,
    /// Most cell formats (`<xf>`), fonts, fills, borders, number formats or differential formats
    /// read from a styles part (each list separately).
    pub styles: usize,
    /// Most shared strings read.
    pub shared_strings: usize,
    /// Most elements and attributes in one XML part read as a tree.
    pub xml_nodes: usize,
    /// Most bytes one zip entry may inflate to, and most compressed bytes read from one entry.
    pub entry_bytes: u64,
    /// Inflating must give at least one byte out per this many bytes in (an entry of empty
    /// compressed blocks gives nothing and costs a lot of time).
    pub min_inflate_ratio: u32,
}

impl Limits {
    /// Upstream's behaviour: arrays up to 10 million cells, no work or time limit.
    pub const UNLIMITED: Limits = Limits {
        array_cells: 10_000_000,
        work: u64::MAX,
        seconds: None,
        styles: usize::MAX,
        shared_strings: usize::MAX,
        xml_nodes: usize::MAX,
        entry_bytes: u64::MAX,
        min_inflate_ratio: u32::MAX,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::UNLIMITED
    }
}

/// A limit that ran out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exceeded {
    Work,
    Time,
}

struct State {
    limits: Limits,
    used: u64,
    exceeded: Option<Exceeded>,
    #[cfg(not(target_family = "wasm"))]
    started: std::time::Instant,
}

impl State {
    fn new(limits: Limits) -> State {
        State {
            limits,
            used: 0,
            exceeded: None,
            #[cfg(not(target_family = "wasm"))]
            started: std::time::Instant::now(),
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new(Limits::UNLIMITED));
    /// Units counted since the clock was last read.
    static SINCE_CLOCK: Cell<u64> = const { Cell::new(0) };
}

/// Sets the limits for the calls that follow on this thread, and starts counting work and time
/// from zero.
pub fn set(limits: Limits) {
    STATE.with(|s| *s.borrow_mut() = State::new(limits));
    SINCE_CLOCK.with(|c| c.set(0));
}

/// The limits in force on this thread.
pub fn get() -> Limits {
    STATE.with(|s| s.borrow().limits)
}

/// Most cells one array result may hold.
pub fn array_cells() -> u64 {
    get().array_cells
}

/// Counts `units` of work. `false` when the work or the time has run out (now or before).
pub fn charge(units: u64) -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.exceeded.is_some() {
            return false;
        }
        s.used = s.used.saturating_add(units);
        if s.used > s.limits.work {
            s.exceeded = Some(Exceeded::Work);
            return false;
        }
        #[cfg(not(target_family = "wasm"))]
        if let Some(max) = s.limits.seconds {
            // Reading the clock costs more than counting: about every 4,096 units.
            let since = SINCE_CLOCK.with(|c| {
                let n = c.get().saturating_add(units);
                c.set(if n >= CLOCK_EVERY { 0 } else { n });
                n
            });
            if since >= CLOCK_EVERY && s.started.elapsed().as_secs_f64() > max {
                s.exceeded = Some(Exceeded::Time);
                return false;
            }
        }
        true
    })
}

/// Units counted between two readings of the clock.
const CLOCK_EVERY: u64 = 4_096;

/// Reads the clock now: `false` when the time (or the work) has run out. For work that is not
/// counted in units, such as unpacking and parsing a file.
pub fn check_time() -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.exceeded.is_some() {
            return false;
        }
        #[cfg(not(target_family = "wasm"))]
        if let Some(max) = s.limits.seconds
            && s.started.elapsed().as_secs_f64() > max
        {
            s.exceeded = Some(Exceeded::Time);
            return false;
        }
        true
    })
}

/// Whether a limit has run out since [`set`].
pub fn exceeded() -> Option<Exceeded> {
    STATE.with(|s| s.borrow().exceeded)
}

/// Units of work counted since [`set`].
pub fn used() -> u64 {
    STATE.with(|s| s.borrow().used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_runs_out_and_stays_out() {
        set(Limits { work: 10, ..Limits::UNLIMITED });
        assert!(charge(6));
        assert!(!charge(6));
        assert_eq!(exceeded(), Some(Exceeded::Work));
        assert!(!charge(0));
        set(Limits::UNLIMITED);
        assert_eq!(exceeded(), None);
        assert!(charge(u64::MAX));
    }
}
