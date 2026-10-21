//! A fuzzing campaign: seeds first, then mutants, each in a child process;
//! failures are confirmed, minimised, written and printed.

pub mod child;
pub mod minimize;
pub mod paths;
pub mod report;
pub mod sink;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use child::{Kind, Limits, Outcome, run_case};
use minimize::{Budget, minimize};
use report::Finding;
use sink::stdout_line;

use crate::say;

use crate::mutate::{Mutant, Mutator, Seed};
use crate::targets::Target;

/// Everything a campaign needs.
#[derive(Debug, Clone)]
pub struct Options {
    pub target: &'static Target,
    /// The fuzzlite binary to run as the child.
    pub exe: PathBuf,
    pub seeds: Vec<Seed>,
    /// Stop after this long...
    pub time: Duration,
    /// ...or after this many cases.
    pub runs: Option<u64>,
    pub jobs: usize,
    pub rng_seed: u64,
    pub limits: Limits,
    pub out: PathBuf,
    /// Minimise findings (`None` to keep them as found).
    pub minimize: Option<Budget>,
    pub max_len: usize,
    /// Time and memory findings kept (different mutation sets); panics and
    /// aborts are never capped.
    pub max_resource_findings: usize,
}

/// What a campaign did.
#[derive(Debug, Default)]
pub struct Summary {
    pub cases: u64,
    pub findings: Vec<Finding>,
    /// Failures that did not happen again on a second run.
    pub flaky: u64,
    /// Failures with a signature already reported.
    pub repeats: u64,
    /// Time and memory signatures not kept because
    /// `max_resource_findings` was reached.
    pub dropped_signatures: u64,
    /// Cases with those signatures (one signature can come up many times).
    pub dropped_at_cap: u64,
    /// Cases where a reader caught a panic inside the `cfb` crate, as
    /// designed.
    pub caught_cfb: u64,
    /// The slowest case that passed: (milliseconds, input bytes, mutation).
    pub slowest: Option<(u64, usize, String)>,
    /// The most memory a passing case used, in KiB.
    pub max_peak_kib: u64,
}

#[derive(Default)]
struct Shared {
    signatures: HashSet<String>,
    /// Signatures dropped at the cap.
    dropped: HashSet<String>,
    resource_findings: usize,
    summary: Summary,
}

/// Checks that a child process starts and reports back.
pub fn check_child(exe: &std::path::Path, limits: &Limits) -> Result<(), String> {
    let out = run_case(exe, "selftest-ok", b"", limits)
        .map_err(|e| format!("cannot start the child process: {e}"))?;
    if out.kind == Kind::Ok {
        Ok(())
    } else {
        Err(format!(
            "the child process did not report back ({}: {})",
            out.kind.as_str(),
            out.detail
        ))
    }
}

/// Runs the campaign. Findings are printed as they are found.
pub fn campaign(opts: &Options) -> Result<Summary, String> {
    if opts.seeds.is_empty() {
        return Err(format!("target {} has no seed inputs", opts.target.name));
    }
    check_child(&opts.exe, &opts.limits)?;
    let next = AtomicU64::new(0);
    let shared = Mutex::new(Shared::default());
    let start = Instant::now();
    let deadline = start + opts.time;
    std::thread::scope(|scope| {
        for worker in 0..opts.jobs.max(1) {
            let (next, shared) = (&next, &shared);
            scope.spawn(move || worker_loop(opts, worker as u64, next, shared, start, deadline));
        }
    });
    let mut shared = shared.into_inner().unwrap_or_else(|e| e.into_inner());
    shared.summary.cases = next
        .load(Ordering::Relaxed)
        .min(opts.runs.unwrap_or(u64::MAX));
    Ok(shared.summary)
}

fn worker_loop(
    opts: &Options,
    worker: u64,
    next: &AtomicU64,
    shared: &Mutex<Shared>,
    start: Instant,
    deadline: Instant,
) {
    let worker_seed = opts.rng_seed ^ worker.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut mutator = Mutator::new(worker_seed, opts.target.shapes, opts.max_len);
    let mut last_progress = Instant::now();
    loop {
        if Instant::now() >= deadline {
            break;
        }
        let case = next.fetch_add(1, Ordering::Relaxed);
        if opts.runs.is_some_and(|r| case >= r) {
            break;
        }
        let n = opts.seeds.len();
        let (seed, mutant) = if (case as usize) < n {
            let s = &opts.seeds[case as usize];
            let m = Mutant {
                bytes: s.bytes.clone(),
                ops: Vec::new(),
            };
            (s, m)
        } else {
            let a = mutator.rng().below(n);
            let b = mutator.rng().below(n);
            (
                &opts.seeds[a],
                mutator.mutate(&opts.seeds[a], &opts.seeds[b]),
            )
        };
        let outcome = match run_case(&opts.exe, opts.target.name, &mutant.bytes, &opts.limits) {
            Ok(o) => o,
            Err(e) => {
                say!("fuzzlite: case {case}: cannot run the child: {e}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if outcome.kind == Kind::Ok {
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            let sum = &mut s.summary;
            sum.caught_cfb += u64::from(outcome.caught_cfb);
            sum.max_peak_kib = sum.max_peak_kib.max(outcome.peak_kib.unwrap_or(0));
            if sum
                .slowest
                .as_ref()
                .is_none_or(|(ms, _, _)| outcome.millis > *ms)
            {
                sum.slowest = Some((outcome.millis, mutant.bytes.len(), mutant.family()));
            }
        } else {
            handle(opts, shared, case, seed, &mutant, outcome);
        }
        if worker == 0 && last_progress.elapsed() >= Duration::from_secs(30) {
            last_progress = Instant::now();
            let done = next.load(Ordering::Relaxed);
            let secs = start.elapsed().as_secs_f64().max(0.001);
            let found = shared.lock().map(|s| s.summary.findings.len()).unwrap_or(0);
            say!(
                "fuzzlite: {done} cases in {secs:.0} s ({:.0}/s), {found} findings",
                done as f64 / secs
            );
        }
    }
}

fn signature(outcome: &Outcome, mutant: &Mutant) -> String {
    match outcome.kind {
        Kind::Panic | Kind::CaughtPanic => format!(
            "{} {}",
            outcome.kind.as_str(),
            outcome.location.as_deref().unwrap_or("")
        ),
        Kind::Abort => {
            let words: String = outcome
                .detail
                .chars()
                .filter(|c| !c.is_ascii_digit())
                .take(80)
                .collect();
            format!("abort {words}")
        }
        // Timeout, memory limit and amplification of one input are one bug
        // seen through a busy or quiet machine: one signature for all three.
        _ => format!("resource {}", mutant.family()),
    }
}

fn handle(
    opts: &Options,
    shared: &Mutex<Shared>,
    case: u64,
    seed: &Seed,
    mutant: &Mutant,
    outcome: Outcome,
) {
    let sig = signature(&outcome, mutant);
    {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        if s.signatures.contains(&sig) {
            s.summary.repeats += 1;
            return;
        }
        let resource = outcome.kind.is_resource();
        if resource && s.resource_findings >= opts.max_resource_findings {
            s.summary.dropped_at_cap += 1;
            if s.dropped.insert(sig) {
                s.summary.dropped_signatures += 1;
            }
            return;
        }
        s.signatures.insert(sig.clone());
        s.resource_findings += usize::from(resource);
    }
    let run = |input: &[u8]| run_case(&opts.exe, opts.target.name, input, &opts.limits).ok();
    // A loaded machine can make a case slow once; ask for a second time.
    let again = run(&mutant.bytes);
    if !again.as_ref().is_some_and(|a| outcome.same_failure(a)) {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.signatures.remove(&sig);
        if outcome.kind.is_resource() {
            s.resource_findings -= 1;
        }
        s.summary.flaky += 1;
        return;
    }
    let (input, last) = match opts.minimize {
        Some(budget) => {
            let mut last = None;
            let small = minimize(&mutant.bytes, budget, |candidate| match run(candidate) {
                Some(o) if outcome.same_failure(&o) => {
                    last = Some((candidate.len(), o));
                    true
                }
                _ => false,
            });
            let last = match last {
                Some((len, o)) if len == small.len() => Some(o),
                _ => None,
            };
            (small, last)
        }
        None => (mutant.bytes.clone(), None),
    };
    // Measure what is reported on the input that is written.
    let measured = last
        .or_else(|| (input == mutant.bytes).then(|| again.clone()).flatten())
        .or_else(|| run(&input))
        .unwrap_or(outcome.clone());
    let kind = if measured.kind.is_resource() && outcome.kind.is_resource() {
        measured.kind
    } else {
        outcome.kind
    };
    let detail = match (&outcome.location, kind) {
        (Some(loc), Kind::Panic | Kind::CaughtPanic) => format!("{loc}: {}", outcome.detail),
        _ => measured.detail.clone(),
    };
    let mut finding = Finding {
        target: opts.target.name.to_string(),
        kind,
        detail,
        family: mutant.family(),
        ops: mutant.ops.iter().map(|s| s.to_string()).collect(),
        seed_file: seed.name.clone(),
        rng_seed: opts.rng_seed,
        case,
        original_bytes: mutant.bytes.len(),
        input,
        millis: measured.millis,
        peak_kib: measured.peak_kib,
        file: None,
    };
    if let Err(e) = finding.write(&opts.out, &mutant.bytes) {
        say!("fuzzlite: cannot write the finding: {e}");
    }
    stdout_line(&finding.json());
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    s.summary.findings.push(finding);
}
