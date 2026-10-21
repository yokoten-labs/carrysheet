//! The process-wide memory budget.
//!
//! SQLite's hard heap limit covers every connection in the process, and
//! `PRAGMA hard_heap_limit` can only lower it, never raise or clear it. So the
//! first engine sets one budget for the process ([`Limits::memory_budget`])
//! and it never changes. Each live engine reserves an estimate of its loaded
//! data; a load is refused when the data of all engines plus the largest
//! `max_memory` of any of them would not fit. Queries and loads run one at a
//! time ([`run_lock`]), so the running query really has its `max_memory`.
//!
//! An engine in a worker process ([`Reservation::for_worker`]) is charged
//! here too, though its memory is the worker's: its data, the copy of the
//! load the parent keeps to restart the worker, and its own `max_memory`,
//! because its queries run beside every other engine's.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Instant;

use rusqlite::Connection;

use crate::error::{QueryError, QueryErrorCode};
use crate::output::Limits;

const MIB: u64 = 1024 * 1024;

/// Memory one engine's connection takes besides its data (schema, caches).
pub(crate) const ENGINE_OVERHEAD: u64 = 2 * MIB;

struct State {
    /// The budget in bytes; 0 until the first engine sets it.
    budget: u64,
    /// Keeps the connection that set the limit, so it is never set again
    /// from a short-lived one.
    control: Option<Connection>,
    /// Live engines: id, data estimate, max_memory.
    engines: Vec<(u64, u64, u64)>,
    next_id: u64,
}

static STATE: Mutex<State> = Mutex::new(State {
    budget: 0,
    control: None,
    engines: Vec::new(),
    next_id: 0,
});

/// True while a query or a load runs; the condition variable wakes the next.
static RUNNING: Mutex<bool> = Mutex::new(false);
static RUN_DONE: Condvar = Condvar::new();

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The right to run, held until dropped.
#[derive(Debug)]
pub(crate) struct Running(());

impl Drop for Running {
    fn drop(&mut self) {
        *RUNNING.lock().unwrap_or_else(|e| e.into_inner()) = false;
        RUN_DONE.notify_one();
    }
}

/// Waits until no other query or load runs, but not past `deadline`:
/// `None` when another one was still running then.
pub(crate) fn run_lock(deadline: Instant) -> Option<Running> {
    let mut busy = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    while *busy {
        let left = deadline.checked_duration_since(Instant::now())?;
        busy = match RUN_DONE.wait_timeout(busy, left) {
            Ok((guard, _)) => guard,
            Err(e) => e.into_inner().0,
        };
    }
    *busy = true;
    Some(Running(()))
}

/// One engine's share of the budget, given back when dropped.
#[derive(Debug)]
pub(crate) struct Reservation {
    id: u64,
}

impl Reservation {
    /// Reserves `data` bytes for an engine whose queries may use
    /// `limits.max_memory`; sets the budget if this is the first engine.
    pub(crate) fn new(data: u64, limits: &Limits) -> Result<Reservation, QueryError> {
        Reservation::add(data, limits.max_memory, limits)
    }

    /// Reserves `data` bytes for an engine in a worker process: the caller
    /// includes the worker's `max_memory` in `data`, since that engine's
    /// queries do not wait for the others.
    pub(crate) fn for_worker(data: u64, limits: &Limits) -> Result<Reservation, QueryError> {
        Reservation::add(data, 0, limits)
    }

    fn add(data: u64, max_memory: u64, limits: &Limits) -> Result<Reservation, QueryError> {
        if limits.memory_budget == 0 {
            return Err(QueryError::new(
                QueryErrorCode::InvalidInput,
                "the memory budget for queries is 0",
                "Limits::memory_budget is the memory all loaded sheets and the running query share; with 0 nothing could load.",
                "Leave memory_budget at its default (512 MiB) or set a size in bytes.",
            ));
        }
        let mut st = state();
        if st.budget == 0 {
            let conn = Connection::open_in_memory().map_err(|e| setup_error(&e))?;
            let bytes = i64::try_from(limits.memory_budget).unwrap_or(i64::MAX);
            conn.query_row(&format!("PRAGMA hard_heap_limit = {bytes}"), [], |_| Ok(()))
                .map_err(|e| setup_error(&e))?;
            st.budget = limits.memory_budget;
            st.control = Some(conn);
        }
        check(&st, None, data, max_memory)?;
        let id = st.next_id;
        st.next_id += 1;
        st.engines.push((id, data, max_memory));
        Ok(Reservation { id })
    }

    /// Replaces the estimate with the measured size after loading; refuses
    /// (and keeps the old estimate) when it no longer fits.
    pub(crate) fn update(&self, data: u64) -> Result<(), QueryError> {
        let mut st = state();
        let max_memory = st
            .engines
            .iter()
            .find(|e| e.0 == self.id)
            .map_or(0, |e| e.2);
        check(&st, Some(self.id), data, max_memory)?;
        if let Some(e) = st.engines.iter_mut().find(|e| e.0 == self.id) {
            e.1 = data;
        }
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // Only bookkeeping: the SQLite limit itself never changes.
        state().engines.retain(|e| e.0 != self.id);
    }
}

/// Fails when the data of all engines (with `data` for this one, leaving out
/// `skip`) plus the largest `max_memory` does not fit in the budget.
fn check(st: &State, skip: Option<u64>, data: u64, max_memory: u64) -> Result<(), QueryError> {
    let others = st.engines.iter().filter(|e| Some(e.0) != skip);
    let other_data: u64 = others.clone().map(|e| e.1).sum();
    let largest_query = others.map(|e| e.2).max().unwrap_or(0).max(max_memory);
    let need = other_data
        .saturating_add(data)
        .saturating_add(largest_query);
    if need <= st.budget {
        return Ok(());
    }
    Err(QueryError::new(
        QueryErrorCode::TooLarge,
        format!(
            "not enough memory to load these sheets for queries: they need about {} MiB",
            (data + max_memory).div_ceil(MIB)
        ),
        format!(
            "Queries share a memory budget of {} MiB per process; {} MiB of it holds sheets already loaded (with the query memory of each worker process){}.",
            st.budget / MIB,
            other_data.div_ceil(MIB),
            if largest_query > 0 {
                format!(
                    ", and a query in this process may use {} MiB",
                    largest_query / MIB
                )
            } else {
                String::new()
            }
        ),
        "Query fewer sheets or a smaller range, or close other loaded workbooks first. The budget is set by the first load (Limits::memory_budget).",
    ))
}

fn setup_error(err: &rusqlite::Error) -> QueryError {
    QueryError::new(
        QueryErrorCode::Internal,
        format!("the memory limit for queries could not be set: {err}"),
        "This is a bug in Carrysheet, or the machine is out of memory.",
        "Try again, and report the problem if it stays.",
    )
}
