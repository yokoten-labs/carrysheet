//! The parent's side: start the worker, send it requests, and kill it when
//! a reply does not come by the deadline.
//!
//! Each worker has three threads here: one writes requests to its stdin,
//! one reads replies from its stdout, one keeps the start of its stderr. So
//! the thread that asks only ever waits on a channel, with a deadline, and
//! a worker that stops reading or stops answering is killed on time.

use std::io::{self, BufReader, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::Worker;
use super::frame::{self, Loaded, Reply};
use crate::engine::budget::{ENGINE_OVERHEAD, Reservation};
use crate::engine::{estimate_bytes, time_limit_error};
use crate::error::{QueryError, QueryErrorCode};
use crate::input::{TableInfo, TableInput};
use crate::output::{Limits, Page, QueryResult};

/// How long after its time limit a query (or a load) may still answer
/// before its worker is killed. The worker stops most queries itself at the
/// limit, with a clearer error; this covers the steps SQLite cannot
/// interrupt.
pub(crate) const GRACE: Duration = Duration::from_millis(200);

/// How long a worker may take to exit after its stdin closes.
const EXIT_WAIT: Duration = Duration::from_millis(200);

/// How often the parent looks at a busy worker's memory (Linux).
const SAMPLE_EVERY: Duration = Duration::from_millis(20);

/// Bytes of a worker's stderr kept for error messages; the rest is read
/// and dropped, so the worker never blocks on a full pipe.
const STDERR_KEPT: usize = 4096;

/// The environment variable that tells a worker its parent's process id, so
/// it can exit when the parent is already gone (Linux).
pub(crate) const PARENT_ENV: &str = "CARRYSHEET_QUERY_PARENT";

/// Loaded tables in a worker process.
pub(crate) struct Remote {
    worker: Worker,
    /// The load request, kept to start a new worker after one is killed.
    /// Charged to the memory budget with the engine.
    load: Arc<Vec<u8>>,
    caps: Caps,
    limits: Limits,
    tables: Vec<TableInfo>,
    /// True while a query runs; queries on one engine run one at a time.
    busy: Mutex<bool>,
    done: Condvar,
    /// The idle worker; `None` while a query runs, and after one was killed
    /// until the next query.
    live: Mutex<Option<Live>>,
    /// The running worker's process id, 0 when none runs.
    pid: AtomicU32,
    _reservation: Reservation,
}

/// The largest replies the parent reads.
#[derive(Debug, Clone, Copy)]
struct Caps {
    loaded: usize,
    reply: usize,
}

/// Why a worker is being loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loading {
    First,
    /// Again, after the last worker was killed or died.
    Again,
}

impl Remote {
    /// Charges the engine to the memory budget, then starts a worker and
    /// loads `inputs` into it.
    pub(crate) fn start(
        worker: Worker,
        inputs: &[TableInput],
        limits: Limits,
    ) -> Result<Remote, QueryError> {
        let load = Arc::new(frame::encode_load(&limits, inputs));
        // The worker's data, the copy kept here, and the worker's own query
        // memory: its queries do not wait for other engines'.
        let charge = |data: u64| {
            data.saturating_add(ENGINE_OVERHEAD)
                .saturating_add(load.len() as u64)
                .saturating_add(limits.max_memory)
        };
        let reservation = Reservation::for_worker(charge(estimate_bytes(inputs)), &limits)?;
        let caps = Caps {
            loaded: frame::loaded_cap(inputs),
            reply: frame::reply_cap(&limits),
        };
        let (live, loaded) = Live::start(&worker, &load, &limits, caps, Loading::First)?;
        if let Err(e) = reservation.update(charge(loaded.bytes)) {
            live.close();
            return Err(e);
        }
        let pid = live.process.id();
        Ok(Remote {
            worker,
            load,
            caps,
            limits,
            tables: loaded.tables,
            busy: Mutex::new(false),
            done: Condvar::new(),
            live: Mutex::new(Some(live)),
            pid: AtomicU32::new(pid),
            _reservation: reservation,
        })
    }

    pub(crate) fn tables(&self) -> &[TableInfo] {
        &self.tables
    }

    /// The running worker's process id, if one runs.
    pub(crate) fn pid(&self) -> Option<u32> {
        match self.pid.load(Ordering::SeqCst) {
            0 => None,
            pid => Some(pid),
        }
    }

    fn live(&self) -> MutexGuard<'_, Option<Live>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs one query. Waiting for another query on this engine counts
    /// toward `max_time`. When the last worker was killed, the sheets are
    /// loaded into a new one first, within `max_load_time` of its own; the
    /// query's time starts when that worker is ready.
    pub(crate) fn query(&self, sql: &str, page: Page) -> Result<QueryResult, QueryError> {
        let start = Instant::now();
        let Some(_turn) = self.wait_turn(start + self.limits.max_time) else {
            return Err(QueryError::new(
                QueryErrorCode::TimeLimit,
                format!(
                    "the query did not start within {} ms",
                    self.limits.max_time.as_millis()
                ),
                "Another query on these sheets was running the whole time; queries run one at a time, and waiting counts toward the time limit.",
                "Try again when the other query has finished.",
            ));
        };
        let waited = start.elapsed();
        let idle = self.live().take();
        let mut live = match idle {
            Some(mut live) => {
                if live.running() {
                    live
                } else {
                    // Killed from outside, or crashed.
                    live.stop();
                    self.reload()?
                }
            }
            // The last query's worker was killed.
            None => self.reload()?,
        };
        let deadline = Instant::now() + self.limits.max_time.saturating_sub(waited) + GRACE;
        let request = Arc::new(frame::encode_query(sql, page));
        let asked = live.ask(request, deadline, true);
        match asked {
            Asked::Reply(Reply::Result(result)) => {
                *self.live() = Some(live);
                Ok(result)
            }
            Asked::Reply(Reply::Error(e)) => {
                *self.live() = Some(live);
                Err(e)
            }
            Asked::Reply(Reply::Loaded(_)) | Asked::BadReply => {
                self.pid.store(0, Ordering::SeqCst);
                let said = live.stop().said;
                Err(bad_reply(&said))
            }
            Asked::Late => {
                self.pid.store(0, Ordering::SeqCst);
                live.stop();
                Err(time_limit_error(&self.limits, page.offset))
            }
            Asked::Died => {
                self.pid.store(0, Ordering::SeqCst);
                let memory = live.memory();
                Err(died(&live.stop(), memory))
            }
        }
    }

    /// Starts a new worker with the same sheets.
    fn reload(&self) -> Result<Live, QueryError> {
        self.pid.store(0, Ordering::SeqCst);
        let (live, _) = Live::start(
            &self.worker,
            &self.load,
            &self.limits,
            self.caps,
            Loading::Again,
        )?;
        self.pid.store(live.process.id(), Ordering::SeqCst);
        Ok(live)
    }

    /// Waits until no other query runs on this engine, but not past
    /// `deadline`.
    fn wait_turn(&self, deadline: Instant) -> Option<Turn<'_>> {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy {
            let left = deadline.checked_duration_since(Instant::now())?;
            busy = match self.done.wait_timeout(busy, left) {
                Ok((guard, _)) => guard,
                Err(e) => e.into_inner().0,
            };
        }
        *busy = true;
        Some(Turn(self))
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        if let Some(live) = self.live().take() {
            live.close();
        }
    }
}

/// The right to run a query, held until dropped.
struct Turn<'a>(&'a Remote);

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        *self.0.busy.lock().unwrap_or_else(|e| e.into_inner()) = false;
        self.0.done.notify_one();
    }
}

/// A running worker.
struct Live {
    process: Child,
    /// Requests for the writer thread; dropping it closes the worker's stdin.
    requests: Option<Sender<Arc<Vec<u8>>>>,
    /// Frames the reader thread got from the worker's stdout.
    replies: Receiver<io::Result<Vec<u8>>>,
    /// The start of the worker's stderr.
    stderr: Arc<Mutex<Vec<u8>>>,
    threads: Vec<JoinHandle<()>>,
    max_memory: u64,
    /// Resident memory after loading, and at the last look during a query.
    base_rss: Option<u64>,
    last_rss: Option<u64>,
}

enum Asked {
    Reply(Reply),
    /// No reply by the deadline.
    Late,
    /// A reply that could not be read: too large, not JSON, an unknown tag.
    BadReply,
    /// The worker exited, or its pipes closed.
    Died,
}

/// What a stopped worker left: how it ended and the start of its stderr.
struct Ended {
    status: Option<ExitStatus>,
    said: String,
}

/// A worker's memory when it died: resident bytes after loading and at the
/// last look, and the query memory it was allowed.
#[derive(Debug, Clone, Copy)]
struct Memory {
    base_rss: Option<u64>,
    last_rss: Option<u64>,
    max_memory: u64,
}

impl Live {
    /// Starts a worker and loads the sheets into it, within
    /// `max_load_time` (plus [`GRACE`]): a worker that does not read the
    /// load or does not answer by then is killed.
    fn start(
        worker: &Worker,
        load: &Arc<Vec<u8>>,
        limits: &Limits,
        caps: Caps,
        loading: Loading,
    ) -> Result<(Live, Loaded), QueryError> {
        let start = Instant::now();
        let mut live = Live::spawn(worker, caps, limits.max_memory).map_err(|e| {
            QueryError::new(
                QueryErrorCode::Internal,
                format!("the query worker could not be started: {e}"),
                "Queries run in a separate process, started from this program, so a runaway query can be stopped.",
                "Check that the program file is still there and may be run, and report the problem if it stays.",
            )
        })?;
        match live.ask(
            Arc::clone(load),
            start + limits.max_load_time + GRACE,
            false,
        ) {
            Asked::Reply(Reply::Loaded(loaded)) => {
                live.base_rss = rss(live.process.id());
                Ok((live, loaded))
            }
            Asked::Reply(Reply::Error(e)) => {
                live.close();
                Err(e)
            }
            Asked::Reply(Reply::Result(_)) | Asked::BadReply => Err(bad_reply(&live.stop().said)),
            Asked::Late => {
                live.stop();
                Err(load_too_slow(limits, loading))
            }
            Asked::Died => {
                let memory = live.memory();
                Err(died(&live.stop(), memory))
            }
        }
    }

    fn spawn(worker: &Worker, caps: Caps, max_memory: u64) -> io::Result<Live> {
        let mut command = Command::new(&worker.program);
        command
            .args(&worker.args)
            // One malloc arena: each extra thread's arena reserves 64 MiB of
            // address space, which counts against the worker's memory cap.
            .env("MALLOC_ARENA_MAX", "1")
            .env(PARENT_ENV, std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = spawn_child(command)?;
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) = (
            process.stdin.take(),
            process.stdout.take(),
            process.stderr.take(),
        ) else {
            let _ = process.kill();
            let _ = process.wait();
            return Err(io::Error::other("the worker has no pipes"));
        };
        let mut threads = Vec::new();
        let mut start_thread = |name: &str, run: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name.into())
                .spawn(run)
                .map(|t| threads.push(t))
        };
        let (send_request, requests) = mpsc::channel::<Arc<Vec<u8>>>();
        let (send_reply, replies) = mpsc::channel();
        let said = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&said);
        let started = start_thread(
            "query-worker-writer",
            Box::new(move || {
                for request in requests {
                    if frame::write(&mut stdin, &request).is_err() {
                        break;
                    }
                }
            }),
        )
        .and_then(|()| {
            start_thread(
                "query-worker-reader",
                Box::new(move || {
                    let mut stdout = BufReader::new(stdout);
                    let mut max = caps.loaded;
                    loop {
                        let frame = match frame::read(&mut stdout, max) {
                            Ok(Some(body)) => Ok(body),
                            Ok(None) => Err(io::ErrorKind::UnexpectedEof.into()),
                            Err(e) => Err(e),
                        };
                        let end = frame.is_err();
                        if send_reply.send(frame).is_err() || end {
                            break;
                        }
                        max = caps.reply;
                    }
                }),
            )
        })
        .and_then(|()| {
            start_thread(
                "query-worker-stderr",
                Box::new(move || {
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = stderr.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                        let room = STDERR_KEPT.saturating_sub(kept.len());
                        kept.extend_from_slice(&buf[..n.min(room)]);
                    }
                }),
            )
        });
        let live = Live {
            process,
            requests: Some(send_request),
            replies,
            stderr: said,
            threads,
            max_memory,
            base_rss: None,
            last_rss: None,
        };
        if let Err(e) = started {
            live.stop();
            return Err(e);
        }
        Ok(live)
    }

    /// Sends one request and waits for its reply until `deadline`; when
    /// `watch` is set, looks at the worker's memory while it waits.
    fn ask(&mut self, request: Arc<Vec<u8>>, deadline: Instant, watch: bool) -> Asked {
        let Some(requests) = self.requests.as_ref() else {
            return Asked::Died;
        };
        // The writer thread sends it; a worker that does not read blocks
        // that thread, never this one.
        if requests.send(request).is_err() {
            return Asked::Died;
        }
        self.last_rss = None;
        loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Asked::Late;
            };
            let wait = if watch { left.min(SAMPLE_EVERY) } else { left };
            match self.replies.recv_timeout(wait) {
                Ok(Ok(body)) => {
                    return match frame::decode_reply(&body) {
                        Ok(reply) => Asked::Reply(reply),
                        Err(_) => Asked::BadReply,
                    };
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::InvalidData => return Asked::BadReply,
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => return Asked::Died,
                Err(RecvTimeoutError::Timeout) => {
                    if watch && let Some(now) = rss(self.process.id()) {
                        self.last_rss = Some(now);
                    }
                }
            }
        }
    }

    fn memory(&self) -> Memory {
        Memory {
            base_rss: self.base_rss,
            last_rss: self.last_rss,
            max_memory: self.max_memory,
        }
    }

    /// True while the worker has not exited.
    fn running(&mut self) -> bool {
        matches!(self.process.try_wait(), Ok(None))
    }

    /// Kills the worker and waits for it and its threads, so no zombie is
    /// left.
    fn stop(mut self) -> Ended {
        let _ = self.process.kill();
        self.reap()
    }

    /// Closes the worker's stdin so it exits by itself; kills it if it has
    /// not after a short wait.
    fn close(mut self) {
        drop(self.requests.take());
        let until = Instant::now() + EXIT_WAIT;
        while Instant::now() < until {
            match self.process.try_wait() {
                Ok(Some(_)) => {
                    self.reap();
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
        }
        let _ = self.process.kill();
        self.reap();
    }

    fn reap(&mut self) -> Ended {
        drop(self.requests.take());
        let status = self.process.wait().ok();
        // The worker is gone, so its pipes are closed and every thread ends.
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let said = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        Ended {
            status,
            said: scrub(&said),
        }
    }
}

/// Starts a child from one thread that lives as long as the process, so a
/// worker's "exit with my parent" signal (Linux `PR_SET_PDEATHSIG`, which
/// follows the thread that started the child, not the process) fires only
/// when this process ends. Falls back to the calling thread when that
/// thread cannot be started.
fn spawn_child(command: Command) -> io::Result<Child> {
    type Job = (Command, Sender<io::Result<Child>>);
    static SPAWNER: OnceLock<Option<Sender<Job>>> = OnceLock::new();
    let spawner = SPAWNER.get_or_init(|| {
        let (send, jobs) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("query-worker-spawner".into())
            .spawn(move || {
                for (mut command, back) in jobs {
                    let _ = back.send(command.spawn());
                }
            })
            .ok()
            .map(|_| send)
    });
    let mut command = command;
    let Some(spawner) = spawner else {
        return command.spawn();
    };
    let (back, child) = mpsc::channel();
    if let Err(mpsc::SendError((mut command, _))) = spawner.send((command, back)) {
        return command.spawn();
    }
    child
        .recv()
        .unwrap_or_else(|_| Err(io::Error::other("the worker spawner stopped")))
}

/// Resident memory of a process in bytes (Linux; `None` elsewhere).
fn rss(pid: u32) -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// The start of a worker's stderr for an error message: the first three
/// lines, at most 300 characters, with the home folder shown as `~` and
/// control characters as `?`.
fn scrub(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .filter(|h| h.len() > 1);
    let mut lines: Vec<String> = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(3)
    {
        let line = match &home {
            Some(home) => line.replace(home.as_str(), "~"),
            None => line.to_owned(),
        };
        lines.push(
            line.chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .collect(),
        );
    }
    let joined = lines.join(" / ");
    if joined.chars().count() > 300 {
        let cut: String = joined.chars().take(300).collect();
        format!("{cut}...")
    } else {
        joined
    }
}

/// " The worker said: ...", or nothing when it said nothing.
fn worker_said(said: &str) -> String {
    if said.is_empty() {
        String::new()
    } else {
        format!(" The worker said: {said}")
    }
}

/// The error for a load that ran out of time, or a worker that never read
/// it.
fn load_too_slow(limits: &Limits, loading: Loading) -> QueryError {
    let ms = limits.max_load_time.as_millis();
    match loading {
        Loading::First => QueryError::new(
            QueryErrorCode::TimeLimit,
            format!("loading the sheets for queries took longer than {ms} ms"),
            "The sheets are too large to load in the time allowed (Limits::max_load_time).",
            "Query fewer sheets or a smaller range.",
        ),
        Loading::Again => QueryError::new(
            QueryErrorCode::TimeLimit,
            format!(
                "loading the sheets again for queries took longer than {ms} ms; the query did not run"
            ),
            "The last query on these sheets was stopped by ending its worker process, so this query first loads the sheets into a new one. That load has its own time limit (Limits::max_load_time), and it ran out.",
            "Run the query again, or query fewer sheets or a smaller range.",
        ),
    }
}

/// The error for a worker that answered something the parent could not
/// read; the parent killed it.
fn bad_reply(said: &str) -> QueryError {
    QueryError::new(
        QueryErrorCode::Internal,
        "the query worker sent a reply that couldn't be read",
        format!(
            "The separate process that runs queries answered something too large or malformed, so it was stopped; this is a bug in Carrysheet.{}",
            worker_said(said)
        ),
        "Run the query again (a new worker starts), and report the problem if it stays.",
    )
}

/// The error for a worker that exited by itself in the middle of a request.
fn died(ended: &Ended, memory: Memory) -> QueryError {
    #[cfg(unix)]
    let by_signal = {
        use std::os::unix::process::ExitStatusExt;
        ended.status.and_then(|s| s.signal())
    };
    #[cfg(not(unix))]
    let by_signal: Option<i32> = None;
    const SIGABRT: i32 = 6;
    const SIGKILL: i32 = 9;
    // An allocation over the worker's address-space cap aborts it. The
    // system's out-of-memory killer sends SIGKILL; so can anyone, so that
    // counts as memory only when the worker was near its share at the last
    // look.
    let near_cap = match (memory.base_rss, memory.last_rss) {
        (Some(base), Some(last)) => last >= base.saturating_add(memory.max_memory / 4 * 3),
        _ => false,
    };
    if by_signal == Some(SIGABRT) || (by_signal == Some(SIGKILL) && near_cap) {
        return QueryError::new(
            QueryErrorCode::TooLarge,
            "the query needed more memory than allowed",
            "Queries run in a separate process with a memory cap, and this one went past it.",
            "Narrow the query: filter rows with WHERE before ORDER BY, GROUP BY or DISTINCT, and select fewer or shorter columns.",
        );
    }
    let how = match (by_signal, ended.status) {
        (Some(SIGKILL), _) => "it was killed (SIGKILL) by another program or the system".to_owned(),
        (_, Some(s)) => s.to_string(),
        (_, None) => "unknown status".to_owned(),
    };
    QueryError::new(
        QueryErrorCode::Internal,
        format!("the query worker stopped: {how}"),
        format!(
            "The separate process that runs queries ended in the middle of the query.{}",
            worker_said(&ended.said)
        ),
        "Run the query again (a new worker starts), and report the problem if it stays.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_is_cut_and_scrubbed() {
        assert_eq!(scrub(b""), "");
        assert_eq!(scrub(b"\n\n a \n b\x07\n c \n d"), "a / b? / c");
        assert!(scrub(&[b'x'; 5000]).ends_with("..."));
        assert_eq!(scrub(&[b'x'; 5000]).chars().count(), 303);
        if let Ok(home) = std::env::var("HOME")
            && home.len() > 1
        {
            let said = scrub(format!("panicked at {home}/src/x.rs").as_bytes());
            assert_eq!(said, "panicked at ~/src/x.rs");
        }
    }

    #[test]
    fn only_abort_or_a_kill_near_the_cap_is_memory() {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let ended = |signal: i32| Ended {
                status: Some(ExitStatus::from_raw(signal)),
                said: "boom".into(),
            };
            let memory = |last: u64| Memory {
                base_rss: Some(100),
                last_rss: Some(last),
                max_memory: 1000,
            };
            let code = |signal, last| died(&ended(signal), memory(last)).code;
            assert_eq!(code(6, 100), QueryErrorCode::TooLarge);
            assert_eq!(code(9, 900), QueryErrorCode::TooLarge);
            assert_eq!(code(9, 200), QueryErrorCode::Internal);
            let e = died(&ended(9), memory(200));
            assert!(e.what.contains("SIGKILL"), "{e}");
            assert!(e.why.contains("The worker said: boom"), "{e}");
            assert_eq!(code(15, 900), QueryErrorCode::Internal);
        }
    }
}
