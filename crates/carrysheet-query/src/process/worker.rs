//! The worker's side: load the sheets, then answer queries until the parent
//! closes stdin.

use std::io::{self, BufReader, BufWriter};
use std::process::ExitCode;

use super::frame::{self, Loaded, Reply, Request};
use crate::engine::Local;

/// Runs this process as a query worker, the other end of
/// [`crate::QueryEngine::start`]. A program calls it when it is started
/// with the arguments its [`crate::Worker`] names, before anything else
/// writes to stdout, and exits with what it returns:
///
/// ```no_run
/// fn main() -> std::process::ExitCode {
///     if std::env::args().nth(1).as_deref() == Some("__query-worker") {
///         return carrysheet_query::serve_worker();
///     }
///     // ... the program as usual
/// #   std::process::ExitCode::SUCCESS
/// }
/// ```
///
/// The worker reads requests on stdin and writes replies on stdout, and
/// returns when stdin closes.
#[expect(
    clippy::disallowed_methods,
    reason = "a worker process: stdout carries its replies and nothing else"
)]
pub fn serve_worker() -> ExitCode {
    if !exit_with_parent() {
        return ExitCode::FAILURE;
    }
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    match serve(&mut input, &mut output, true) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// `cap_memory` is false in tests, which run in the test process.
fn serve(
    input: &mut impl io::Read,
    output: &mut impl io::Write,
    cap_memory: bool,
) -> io::Result<()> {
    let Some(body) = frame::read(input, frame::MAX_FRAME)? else {
        return Ok(());
    };
    let Request::Load(limits, inputs) = frame::decode_request(&body)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the first request must be a load",
        ));
    };
    drop(body);
    let engine = Local::load(&inputs, limits.clone());
    drop(inputs);
    let engine = match engine {
        Ok(engine) => engine,
        Err(e) => return frame::write(output, &frame::encode_reply(&Reply::Error(e))),
    };
    if cap_memory {
        limit_memory(limits.max_memory);
    }
    let loaded = Reply::Loaded(Loaded {
        tables: engine.tables().to_vec(),
        bytes: engine.data_bytes(),
    });
    frame::write(output, &frame::encode_reply(&loaded))?;
    while let Some(body) = frame::read(input, frame::MAX_FRAME)? {
        let Request::Query(sql, page) = frame::decode_request(&body)? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "only queries follow a load",
            ));
        };
        let reply = match engine.query(&sql, page) {
            Ok(result) => Reply::Result(result),
            Err(e) => Reply::Error(e),
        };
        frame::write(output, &frame::encode_reply(&reply))?;
    }
    Ok(())
}

/// Asks Linux to kill this worker when the parent dies
/// (`PR_SET_PDEATHSIG`), so a worker never outlives its program by up to
/// a query's time limit. False when the parent is already gone: it may have
/// died before the request took effect. The parent starts every worker from
/// one long-lived thread, since the signal follows that thread.
#[cfg(target_os = "linux")]
fn exit_with_parent() -> bool {
    use rustix::process::{Signal, getppid, set_parent_process_death_signal};
    if set_parent_process_death_signal(Some(Signal::KILL)).is_err() {
        return true;
    }
    let Some(parent) = std::env::var(super::parent::PARENT_ENV)
        .ok()
        .and_then(|p| p.parse::<i32>().ok())
    else {
        // Started by hand or by another program: nothing to compare.
        return true;
    };
    getppid().map(|p| p.as_raw_nonzero().get()) == Some(parent)
}

/// Elsewhere a worker whose parent died notices only when its stdin closes,
/// after the running query.
#[cfg(not(target_os = "linux"))]
fn exit_with_parent() -> bool {
    true
}

/// Address space the worker may add after loading, besides `max_memory`:
/// thread stacks, result pages being written, and allocator slack.
#[cfg(target_os = "linux")]
pub(crate) const MEMORY_SLACK: u64 = 64 * 1024 * 1024;

/// Caps this process's address space (Linux `RLIMIT_AS`) at what it uses
/// now, after loading, plus `max_memory` and [`MEMORY_SLACK`]. SQLite's own
/// heap limit stops it first; this catches what that does not count. A
/// process over the cap fails to allocate and aborts, and the parent
/// reports that the query needed too much memory.
#[cfg(target_os = "linux")]
fn limit_memory(max_memory: u64) {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let Some(now) = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            let line = s.lines().find(|l| l.starts_with("VmSize:"))?;
            line.split_whitespace().nth(1)?.parse::<u64>().ok()
        })
        .map(|kib| kib * 1024)
    else {
        return;
    };
    let cap = now.saturating_add(max_memory).saturating_add(MEMORY_SLACK);
    let old = getrlimit(Resource::As);
    let cap = match old.maximum {
        Some(hard) => cap.min(hard),
        None => cap,
    };
    let _ = setrlimit(
        Resource::As,
        Rlimit {
            current: Some(cap),
            maximum: old.maximum,
        },
    );
}

/// Elsewhere only SQLite's heap limit bounds the worker's memory: Windows
/// needs a job object and macOS does not enforce `RLIMIT_AS`.
#[cfg(not(target_os = "linux"))]
fn limit_memory(_max_memory: u64) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{QueryCell, TableInput};
    use crate::output::{Limits, Page};

    #[test]
    fn serves_a_load_and_queries() {
        let mut input = Vec::new();
        let table = TableInput::new("T", vec![vec!["a".into()], vec![QueryCell::from(2.0)]]);
        frame::write(
            &mut input,
            &frame::encode_load(&Limits::default(), &[table]),
        )
        .unwrap();
        frame::write(
            &mut input,
            &frame::encode_query("SELECT a * 21 FROM T", Page::default()),
        )
        .unwrap();
        frame::write(
            &mut input,
            &frame::encode_query("SELECT nope", Page::default()),
        )
        .unwrap();
        let mut output = Vec::new();
        serve(&mut input.as_slice(), &mut output, false).unwrap();
        let mut r = output.as_slice();
        let mut next = || {
            frame::decode_reply(&frame::read(&mut r, frame::MAX_FRAME).unwrap().unwrap()).unwrap()
        };
        assert!(matches!(next(), Reply::Loaded(l) if l.tables[0].name == "T" && l.bytes > 0));
        assert!(matches!(next(), Reply::Result(r) if r.rows[0][0] == 42));
        assert!(matches!(next(), Reply::Error(e) if e.what.contains("nope")));
    }
}
