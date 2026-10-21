//! Runs one input in a child process (fuzzlite itself, re-executed with a
//! hidden subcommand) under a wall-clock and a memory limit, and measures
//! its time and peak memory.
//!
//! Memory limit: on Linux the child starts through `/bin/sh -c 'ulimit -v'`,
//! which sets RLIMIT_AS without `unsafe` code or a new crate, and the parent
//! also kills a child whose resident memory passes the limit. Elsewhere only
//! the time limit applies (a Windows job object needs `unsafe` FFI).
//!
//! Peak memory is read from `/proc`, so it too is measured on Linux only
//! (see [`MEMORY_MEASURED`]): elsewhere a case that uses too much memory
//! but finishes in time is not an amplification.

use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::sink::stdout_line;
use crate::targets::Target;

/// True where fuzzlite can limit and measure a case's memory (Linux, by
/// `ulimit -v` and `/proc`). Elsewhere `--memory-mib` and `--amp-mib` do
/// nothing, and the commands say so.
pub const MEMORY_MEASURED: bool = cfg!(target_os = "linux");

/// The hidden subcommand the parent runs.
pub const CHILD_COMMAND: &str = "__child";
const REPORT: &str = "fuzzlite-child";

/// Limits for one case.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Kill the child after this long.
    pub timeout: Duration,
    /// Address space (Linux RLIMIT_AS) and resident memory limit.
    pub memory_bytes: u64,
    /// A case that finishes but takes this long is an amplification...
    pub amp_time: Duration,
    /// ...as is one that uses this much memory...
    pub amp_bytes: u64,
    /// ...when its input is smaller than this.
    pub amp_input: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            timeout: Duration::from_secs(5),
            memory_bytes: 1 << 30,
            amp_time: Duration::from_secs(2),
            amp_bytes: 256 << 20,
            amp_input: 1 << 20,
        }
    }
}

/// What happened to one case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Ok,
    Panic,
    /// A panic that a reader caught itself (its `catch_unwind` guards wrap
    /// more than the `cfb` crate), outside `cfb`.
    CaughtPanic,
    Abort,
    Timeout,
    MemoryLimit,
    Amplification,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ok => "ok",
            Kind::Panic => "panic",
            Kind::CaughtPanic => "caught_panic",
            Kind::Abort => "abort",
            Kind::Timeout => "timeout",
            Kind::MemoryLimit => "memory_limit",
            Kind::Amplification => "amplification",
        }
    }

    /// The signature group: `resource` for time and memory (one input can
    /// be a timeout on a busy machine and an amplification on a quiet one),
    /// else the kind.
    pub fn group(self) -> &'static str {
        if self.is_resource() {
            "resource"
        } else {
            self.as_str()
        }
    }

    /// Too much time or memory, as opposed to a crash.
    pub fn is_resource(self) -> bool {
        matches!(
            self,
            Kind::Timeout | Kind::MemoryLimit | Kind::Amplification
        )
    }
}

/// The result of one case.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub kind: Kind,
    /// Where a panic happened (`file:line:col`), when it did.
    pub location: Option<String>,
    pub detail: String,
    pub millis: u64,
    pub peak_kib: Option<u64>,
    /// A panic inside the `cfb` crate was caught by the reader, as designed.
    pub caught_cfb: bool,
}

impl Outcome {
    /// True when `other` is the same failure, for confirming and
    /// minimising: the same panic location, any abort, or any blow-up.
    pub fn same_failure(&self, other: &Outcome) -> bool {
        match self.kind {
            Kind::Ok => other.kind == Kind::Ok,
            Kind::Panic | Kind::CaughtPanic => {
                other.kind == self.kind && other.location == self.location
            }
            Kind::Abort => other.kind == Kind::Abort,
            _ => other.kind.is_resource(),
        }
    }
}

/// Runs `input` through `target` in a child process.
pub fn run_case(
    exe: &Path,
    target: &str,
    input: &[u8],
    limits: &Limits,
) -> std::io::Result<Outcome> {
    let mut cmd = if cfg!(target_os = "linux") && Path::new("/bin/sh").exists() {
        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg("ulimit -v \"$0\" && exec \"$@\"")
            .arg((limits.memory_bytes / 1024).to_string())
            .arg(exe);
        c
    } else {
        Command::new(exe)
    };
    cmd.arg(CHILD_COMMAND)
        .arg(target)
        .arg(limits.timeout.as_millis().to_string())
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let pid = child.id();

    std::thread::scope(|scope| {
        scope.spawn(move || {
            if let Some(w) = stdin.as_mut() {
                let _ = w.write_all(input);
            }
            drop(stdin);
        });
        let out = scope.spawn(move || read_capped(stdout.as_mut()));
        let err = scope.spawn(move || read_capped(stderr.as_mut()));

        let mut killed = None;
        let mut polled_peak = None;
        let mut nap = Duration::from_micros(100);
        let mut next_poll = Duration::ZERO;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    // Never leave the pipe threads waiting on a live child.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
            }
            let elapsed = start.elapsed();
            if elapsed >= limits.timeout {
                killed = Some(Kind::Timeout);
                let _ = child.kill();
                break child.wait()?;
            }
            if elapsed >= next_poll {
                next_poll = elapsed + Duration::from_millis(5);
                if let Some((rss, hwm)) = proc_memory(pid) {
                    polled_peak = polled_peak.max(Some(hwm));
                    if rss.saturating_mul(1024) > limits.memory_bytes {
                        killed = Some(Kind::MemoryLimit);
                        let _ = child.kill();
                        break child.wait()?;
                    }
                }
            }
            std::thread::sleep(nap);
            nap = (nap * 2).min(Duration::from_millis(2));
        };
        let wall = start.elapsed();
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        Ok(classify(
            input.len(),
            limits,
            killed,
            status,
            wall,
            polled_peak,
            &String::from_utf8_lossy(&stdout),
            &String::from_utf8_lossy(&stderr),
        ))
    })
}

/// Reads a pipe to the end, keeping the first 64 KiB.
fn read_capped(pipe: Option<&mut impl Read>) -> Vec<u8> {
    let mut kept = Vec::new();
    let Some(pipe) = pipe else { return kept };
    let mut buf = [0u8; 8192];
    while let Ok(n) = pipe.read(&mut buf) {
        if n == 0 {
            break;
        }
        let room = (64usize << 10).saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
    kept
}

#[allow(clippy::too_many_arguments)]
fn classify(
    input_len: usize,
    limits: &Limits,
    killed: Option<Kind>,
    status: ExitStatus,
    wall: Duration,
    polled_peak: Option<u64>,
    stdout: &str,
    stderr: &str,
) -> Outcome {
    let report = stdout.lines().position(|l| l.starts_with(REPORT));
    let lines: Vec<&str> = stdout.lines().collect();
    let (state, micros, peak) = match report {
        Some(i) => parse_report(lines[i]),
        None => (None, None, None),
    };
    let peak_kib = peak.or(polled_peak);
    let millis = micros.map_or(wall.as_millis() as u64, |m| m / 1000);
    let mut out = Outcome {
        kind: Kind::Ok,
        location: None,
        detail: String::new(),
        millis,
        peak_kib,
        caught_cfb: false,
    };
    if let Some(kind) = killed {
        out.kind = kind;
        out.millis = wall.as_millis() as u64;
        out.detail = match kind {
            Kind::Timeout => format!("still running after {} ms", limits.timeout.as_millis()),
            _ => format!("resident memory passed {} MiB", limits.memory_bytes >> 20),
        };
        return out;
    }
    match state {
        Some(s @ ("ok" | "caught")) if status.success() => {
            if s == "caught" {
                let i = report.unwrap_or(0);
                let location = lines.get(i + 1).map(|s| s.to_string()).unwrap_or_default();
                if is_cfb(&location) {
                    out.caught_cfb = true;
                } else {
                    out.kind = Kind::CaughtPanic;
                    out.detail = lines.get(i + 2..).unwrap_or_default().join("\n");
                    out.location = Some(location);
                    return out;
                }
            }
            let slow = Duration::from_millis(out.millis) >= limits.amp_time;
            let big = peak_kib.is_some_and(|k| k.saturating_mul(1024) >= limits.amp_bytes);
            if input_len <= limits.amp_input && (slow || big) {
                out.kind = Kind::Amplification;
                out.detail = format!(
                    "{} ms and {} MiB peak for a {input_len}-byte input",
                    out.millis,
                    peak_kib.unwrap_or(0).div_ceil(1024)
                );
            }
        }
        Some("panic") => {
            let i = report.unwrap_or(0);
            out.kind = Kind::Panic;
            out.location = lines.get(i + 1).map(|s| s.to_string());
            out.detail = lines.get(i + 2..).unwrap_or_default().join("\n");
        }
        _ => {
            out.millis = wall.as_millis() as u64;
            let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            if status.code() == Some(WATCHDOG_EXIT) {
                out.kind = Kind::Timeout;
                out.detail = format!("the child stopped itself after {} ms", out.millis);
            } else if let Some(line) = stderr.lines().find(|l| l.contains("memory allocation of")) {
                out.kind = Kind::MemoryLimit;
                out.detail = line.trim().to_string();
            } else if signal(status) == Some(9) {
                out.kind = Kind::MemoryLimit;
                out.detail =
                    "killed by SIGKILL, most likely the system's out-of-memory killer".into();
            } else {
                out.kind = Kind::Abort;
                let how = match (signal(status), status.code()) {
                    (Some(s), _) => format!("signal {s}"),
                    (None, Some(c)) => format!("exit code {c}"),
                    _ => "no exit code".into(),
                };
                out.detail = if stderr.contains("overflowed its stack") {
                    format!("stack overflow ({how})")
                } else if first.is_empty() {
                    how
                } else {
                    format!("{how}: {}", first.trim())
                };
            }
        }
    }
    out
}

fn parse_report(line: &str) -> (Option<&str>, Option<u64>, Option<u64>) {
    let mut words = line.split_whitespace().skip(1);
    let state = words.next();
    let mut micros = None;
    let mut peak = None;
    for w in words {
        if let Some(v) = w.strip_prefix("micros=") {
            micros = v.parse().ok();
        } else if let Some(v) = w.strip_prefix("peak_kib=") {
            peak = v.parse().ok();
        }
    }
    (state, micros, peak)
}

#[cfg(unix)]
fn signal(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal(_: ExitStatus) -> Option<i32> {
    None
}

/// (resident, peak resident) in KiB for a process, from /proc.
fn proc_memory(pid: u32) -> Option<(u64, u64)> {
    if !MEMORY_MEASURED {
        return None;
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |name: &str| -> Option<u64> {
        let line = status.lines().find(|l| l.starts_with(name))?;
        line[name.len()..].split_whitespace().next()?.parse().ok()
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

/// The default memory limit per case: half the machine's memory split
/// over the jobs, between 128 MiB and 1 GiB. 1 GiB where the total is
/// unknown (not Linux).
pub fn default_memory_bytes(jobs: usize) -> u64 {
    let total_kib = std::fs::read_to_string("/proc/meminfo").ok().and_then(|m| {
        let line = m.lines().find(|l| l.starts_with("MemTotal:"))?;
        line["MemTotal:".len()..]
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    });
    memory_for(total_kib, jobs)
}

fn memory_for(total_kib: Option<u64>, jobs: usize) -> u64 {
    let cap = 1u64 << 30;
    match total_kib {
        Some(kib) => (kib * 1024 / 2 / jobs.max(1) as u64).clamp(128 << 20, cap),
        None => cap,
    }
}

/// The exit code of a child that stopped itself at its own deadline.
pub const WATCHDOG_EXIT: i32 = 124;

/// True for a panic location inside the `cfb` crate, whose panics the
/// readers catch on purpose.
pub fn is_cfb(location: &str) -> bool {
    location.contains("/cfb-") || location.contains("\\cfb-")
}

/// A panic: (location, message).
type Seen = (String, String);

/// Panics seen in the child, each in its own slot so none crowds out
/// another: (first, first outside `cfb`, latest).
static PANICS: Mutex<(Option<Seen>, Option<Seen>, Option<Seen>)> = Mutex::new((None, None, None));

/// The child side: reads the input from stdin, runs the target, and
/// reports `ok`, `caught` (a panic the reader caught) or `panic`, with its
/// time and peak memory, on stdout. An abort, a stack overflow or a kill
/// leaves no report.
///
/// `timeout_ms` arms a watchdog: if the parent dies and nobody kills the
/// child, it exits with [`WATCHDOG_EXIT`] two seconds after the parent's
/// own deadline instead of spinning forever.
pub fn child_main(target: &Target, timeout_ms: Option<u64>) -> ExitCode {
    if let Some(ms) = timeout_ms {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms.saturating_add(2000)));
            std::process::exit(WATCHDOG_EXIT);
        });
    }
    let mut data = Vec::new();
    if std::io::stdin().read_to_end(&mut data).is_err() {
        return ExitCode::from(2);
    }
    // Readers catch some panics themselves; keep quiet and remember them.
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let mut seen = PANICS.lock().unwrap_or_else(|e| e.into_inner());
        let this = (location, message);
        if seen.0.is_none() {
            seen.0 = Some(this.clone());
        }
        if seen.1.is_none() && !is_cfb(&this.0) {
            seen.1 = Some(this.clone());
        }
        seen.2 = Some(this);
    }));
    let start = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| (target.run)(&data)));
    let micros = start.elapsed().as_micros();
    let peak =
        proc_memory(std::process::id()).map_or(String::from("-"), |(_, hwm)| hwm.to_string());
    let (first, outside_cfb, latest) = PANICS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // The panic to report: the one that escaped (the latest), or the first
    // caught one outside `cfb`, or the first caught one.
    let (state, shown) = if result.is_err() {
        ("panic", latest)
    } else if let Some(p) = outside_cfb.or(first) {
        ("caught", Some(p))
    } else {
        ("ok", None)
    };
    let mut report = format!("{REPORT} {state} micros={micros} peak_kib={peak}");
    if let Some((location, message)) = shown {
        report.push('\n');
        report.push_str(&location);
        report.push('\n');
        report.push_str(&message);
    }
    stdout_line(&report);
    if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(101)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_status() -> ExitStatus {
        ExitStatus::default()
    }

    fn report(state: &str, ms: u64, extra: &str) -> String {
        format!(
            "{REPORT} {state} micros={} peak_kib=1000\n{extra}",
            ms * 1000
        )
    }

    #[test]
    fn a_slow_case_of_exactly_max_len_is_an_amplification() {
        let l = Limits::default();
        let out = classify(
            1 << 20,
            &l,
            None,
            ok_status(),
            Duration::from_secs(5),
            None,
            &report("ok", 4900, ""),
            "",
        );
        assert_eq!(out.kind, Kind::Amplification);
        let out = classify(
            (1 << 20) + 1,
            &l,
            None,
            ok_status(),
            Duration::from_secs(5),
            None,
            &report("ok", 4900, ""),
            "",
        );
        assert_eq!(out.kind, Kind::Ok);
    }

    #[test]
    fn default_memory_is_half_the_machine_over_the_jobs() {
        let gib = 1u64 << 20; // in KiB
        assert_eq!(memory_for(Some(3 * gib), 1), 1 << 30);
        assert_eq!(memory_for(Some(3 * gib), 2), 768 << 20);
        assert_eq!(memory_for(Some(3 * gib), 4), 384 << 20);
        assert_eq!(memory_for(Some(gib / 4), 8), 128 << 20);
        assert_eq!(memory_for(None, 4), 1 << 30);
    }

    #[test]
    fn caught_panics_outside_cfb_are_findings() {
        let l = Limits::default();
        let run = |loc: &str| {
            classify(
                10,
                &l,
                None,
                ok_status(),
                Duration::from_millis(5),
                None,
                &report("caught", 1, &format!("{loc}\nboom")),
                "",
            )
        };
        let out = run("crates/carrysheet-vba/src/dir.rs:10:5");
        assert_eq!(out.kind, Kind::CaughtPanic);
        assert_eq!(out.detail, "boom");
        let out = run("~/.cargo/registry/src/index/cfb-0.15.0/src/lib.rs:1:1");
        assert_eq!(out.kind, Kind::Ok);
        assert!(out.caught_cfb);
    }

    #[test]
    fn a_child_that_stops_itself_is_a_timeout() {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(WATCHDOG_EXIT << 8)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(WATCHDOG_EXIT as u32)
        };
        let out = classify(
            1,
            &Limits::default(),
            None,
            status,
            Duration::from_secs(7),
            None,
            "",
            "",
        );
        assert_eq!(out.kind, Kind::Timeout);
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_from_outside_reads_as_memory_limit() {
        use std::os::unix::process::ExitStatusExt;
        let out = classify(
            1,
            &Limits::default(),
            None,
            ExitStatus::from_raw(9),
            Duration::from_millis(50),
            None,
            "",
            "",
        );
        assert_eq!(out.kind, Kind::MemoryLimit);
        assert!(out.detail.contains("SIGKILL"), "{}", out.detail);
    }
}
