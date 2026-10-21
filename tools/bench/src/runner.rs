//! Running a case: inputs on disk, one child process per case, and the
//! numbers it reports.
//!
//! Each case runs in its own child process (this same binary, `bench
//! child <case>`), so its peak memory is its own: on Linux the child reads
//! its high-water mark (`VmHWM` in `/proc/self/status`) after the last run.
//! Other systems would need `unsafe` or a dependency for that, so peak
//! memory is reported as unavailable there.

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::cases::{Case, Input, all};
use crate::generate::Generated;

/// What one case measured.
#[derive(Debug, Clone)]
pub struct Measured {
    pub name: String,
    /// Nanoseconds per run, one per timed sample, in run order.
    pub samples_ns: Vec<u64>,
    /// Peak resident memory of the child, in KiB, where the system tells.
    pub peak_kib: Option<u64>,
    /// The operation's check value.
    pub check: u64,
    /// Median nanoseconds of the fixed reference workload, timed in its
    /// own process just before and just after the case (the mean of the
    /// two): how fast the machine was while the case ran.
    pub ref_ns: u64,
    /// Total bytes of input files.
    pub input_bytes: u64,
    /// FNV-1a hash of the input files, or `None` for a salted input.
    pub input_hash: Option<String>,
}

impl Measured {
    pub fn median_ns(&self) -> u64 {
        let mut s = self.samples_ns.clone();
        s.sort_unstable();
        let n = s.len();
        if n == 0 {
            0
        } else if n % 2 == 1 {
            s[n / 2]
        } else {
            (s[n / 2 - 1] + s[n / 2]) / 2
        }
    }

    pub fn min_ns(&self) -> u64 {
        self.samples_ns.iter().copied().min().unwrap_or(0)
    }

    pub fn max_ns(&self) -> u64 {
        self.samples_ns.iter().copied().max().unwrap_or(0)
    }

    /// (max - min) / median, as a fraction.
    pub fn spread(&self) -> f64 {
        let m = self.median_ns();
        if m == 0 {
            0.0
        } else {
            (self.max_ns() - self.min_ns()) as f64 / m as f64
        }
    }
}

/// Where inputs live: `--inputs`, else `target/bench-inputs` in the
/// repository.
pub fn inputs_dir(flag: Option<&Path>) -> PathBuf {
    match flag {
        Some(p) => p.to_path_buf(),
        None => repo_root().join("target").join("bench-inputs"),
    }
}

/// The repository root, from where this crate sits in it.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The files a case reads, in a fixed order.
pub fn input_files(input: &Input, dir: &Path) -> io::Result<Vec<PathBuf>> {
    match input {
        Input::None => Ok(Vec::new()),
        Input::Generated(g) => Ok(vec![dir.join(g.file_name())]),
        Input::Files { dirs, exts } => {
            let mut out = Vec::new();
            for d in *dirs {
                walk(&repo_root().join(d), exts, &mut out)?;
            }
            out.sort();
            if out.is_empty() {
                return Err(io::Error::other(format!(
                    "no {} files under {}",
                    exts.join("/"),
                    dirs.join(", ")
                )));
            }
            Ok(out)
        }
    }
}

fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", dir.display())))?
    {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, exts, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Writes a generated input unless it is already there. Returns whether it
/// wrote it.
pub fn ensure_generated(g: &Generated, dir: &Path) -> io::Result<bool> {
    let path = dir.join(g.file_name());
    if path.is_file() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{}.partial", g.file_name()));
    {
        let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
        (g.make)(&mut w)?;
        w.flush()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(true)
}

/// FNV-1a over each file's length and bytes.
pub fn hash_files(files: &[PathBuf]) -> io::Result<(u64, String)> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut total = 0u64;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for f in files {
        let bytes = std::fs::read(f)?;
        total += bytes.len() as u64;
        eat(&(bytes.len() as u64).to_le_bytes());
        eat(&bytes);
    }
    Ok((total, format!("{h:016x}")))
}

/// Runs a case in a child process and collects what it reports.
pub fn run_case(case: &Case, runs: u32, dir: &Path) -> Result<Measured, String> {
    let files = input_files(&case.input, dir).map_err(|e| e.to_string())?;
    let (input_bytes, hash) = hash_files(&files).map_err(|e| e.to_string())?;
    let salted = matches!(case.input, Input::Generated(g) if g.salted);
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the bench binary: {e}"))?;
    let ref_before = reference(&exe)?;
    let out = Command::new(&exe)
        .arg("child")
        .arg(case.name)
        .arg("--runs")
        .arg(runs.to_string())
        .arg("--inputs")
        .arg(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot start the child process: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().last().unwrap_or("");
    let mut parts = line.split(' ');
    match parts.next() {
        Some("ok") => {}
        Some("err") => return Err(line[4..].to_string()),
        _ => {
            return Err(format!(
                "the child process ended without a result ({}); it may have run out of memory",
                out.status
            ));
        }
    }
    let bad = || format!("unreadable child result: {line}");
    let check = parts.next().and_then(|p| p.parse().ok()).ok_or_else(bad)?;
    let peak_kib = match parts.next().ok_or_else(bad)? {
        "-" => None,
        p => Some(p.parse().map_err(|_| bad())?),
    };
    let samples_ns = parts
        .next()
        .ok_or_else(bad)?
        .split(',')
        .map(|s| s.parse().map_err(|_| bad()))
        .collect::<Result<Vec<u64>, _>>()?;
    let ref_ns = (ref_before + reference(&exe)?) / 2;
    Ok(Measured {
        name: case.name.to_string(),
        samples_ns,
        peak_kib,
        check,
        ref_ns,
        input_bytes,
        input_hash: (!salted).then_some(hash),
    })
}

/// Times the reference workload in a child process of its own (so it
/// never adds to a case's peak memory).
fn reference(exe: &Path) -> Result<u64, String> {
    let out = Command::new(exe)
        .arg("reference")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot start the reference process: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .strip_prefix("ok ")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| "the reference process gave no result".to_string())
}

/// The reference side: prints `ok <median ns>` of 5 runs of
/// [`reference_work`].
pub fn reference_child() -> i32 {
    std::hint::black_box(reference_work()); // warm-up
    let mut times: Vec<u64> = (0..5)
        .map(|_| {
            let start = Instant::now();
            std::hint::black_box(reference_work());
            start.elapsed().as_nanos() as u64
        })
        .collect();
    times.sort_unstable();
    crate::out(&format!("ok {}", times[2]));
    0
}

/// A fixed workload like the readers', about 30 ms: format numbers,
/// allocate and free many small strings, hash bytes. It never changes, so
/// its time says how fast the machine is at the moment: a shared machine
/// under load, a laptop that slows when warm, a hybrid CPU with fast and
/// slow cores all show up here as well as in the cases.
fn reference_work() -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for round in 0..10u64 {
        let mut strings = Vec::with_capacity(100_000);
        for i in 0..100_000u64 {
            let s = (i ^ round).wrapping_mul(2_654_435_761).to_string();
            for b in s.bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            strings.push(s);
        }
        h ^= strings.iter().map(|s| s.len() as u64).sum::<u64>();
    }
    h
}

/// The child side: runs one case and prints `ok <check> <peak KiB or -> <ns,...>`
/// or `err <message>`.
pub fn child(name: &str, runs: u32, dir: &Path) -> i32 {
    let result = child_inner(name, runs, dir);
    let line = match &result {
        Ok((check, samples)) => format!(
            "ok {check} {} {}",
            peak_kib().map_or("-".to_string(), |k| k.to_string()),
            samples
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Err(e) => format!("err {e}"),
    };
    crate::out(&line);
    i32::from(result.is_err())
}

fn child_inner(name: &str, runs: u32, dir: &Path) -> Result<(u64, Vec<u64>), String> {
    let case = all()
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("no case named {name}"))?;
    let files = input_files(&case.input, dir).map_err(|e| e.to_string())?;
    let inputs = files
        .iter()
        .map(|f| std::fs::read(f).map_err(|e| format!("{}: {e}", f.display())))
        .collect::<Result<Vec<_>, _>>()?;
    let mut op = (case.setup)(inputs)?;
    // One untimed warm-up sample: page faults, caches, lazy statics.
    let mut check = 0;
    for _ in 0..case.inner {
        check = std::hint::black_box(op()?);
    }
    let mut samples = Vec::with_capacity(runs as usize);
    for _ in 0..runs {
        let start = Instant::now();
        for _ in 0..case.inner {
            let c = std::hint::black_box(op()?);
            if c != check {
                return Err(format!(
                    "the result changed between runs ({check} then {c}); the operation is not repeatable"
                ));
            }
        }
        samples.push((start.elapsed().as_nanos() / u128::from(case.inner)) as u64);
    }
    Ok((check, samples))
}

/// The process's peak resident memory in KiB (Linux `VmHWM`).
fn peak_kib() -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

/// A short description of the machine, to warn when a baseline came from
/// another one. Hardware only; nothing about the person.
pub fn machine() -> String {
    let cpus = std::thread::available_parallelism().map_or(0, |n| n.get());
    let mut model = String::new();
    let mut mem = String::new();
    if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo")
        && let Some(m) = info
            .lines()
            .find_map(|l| l.strip_prefix("model name").and_then(|r| r.split_once(':')))
    {
        model = format!(", {}", m.1.trim());
    }
    if let Ok(info) = std::fs::read_to_string("/proc/meminfo")
        && let Some(kib) = info
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
    {
        mem = format!(", {} GiB RAM", (kib + (1 << 19)) >> 20);
    }
    format!(
        "{}-{}, {cpus} CPUs{model}{mem}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats() {
        let m = Measured {
            name: "x/y".into(),
            samples_ns: vec![50, 10, 30, 20],
            peak_kib: None,
            check: 0,
            ref_ns: 0,
            input_bytes: 0,
            input_hash: None,
        };
        assert_eq!((m.median_ns(), m.min_ns(), m.max_ns()), (25, 10, 50));
        assert!((m.spread() - 1.6).abs() < 1e-9);
    }

    #[test]
    fn committed_inputs_are_found() {
        for case in all() {
            if let Input::Files { .. } = case.input {
                let files = input_files(&case.input, Path::new(".")).unwrap();
                assert!(!files.is_empty(), "{}", case.name);
            }
        }
    }

    #[test]
    fn peak_memory_on_linux() {
        assert_eq!(peak_kib().is_some(), cfg!(target_os = "linux"));
    }
}
