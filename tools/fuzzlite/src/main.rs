//! The fuzzlite command line. See README.md.

// All output goes through `fuzzlite::run::sink`, which hides the home folder.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use fuzzlite::run::child::{self, CHILD_COMMAND, Kind, Limits, run_case};
use fuzzlite::run::minimize::Budget;
use fuzzlite::run::paths::{self, display_path};
use fuzzlite::run::report::JsonLine;
use fuzzlite::run::sink::stdout_line;
use fuzzlite::run::{self as campaign, Options, check_child};
use fuzzlite::say;
use fuzzlite::{seeds, targets};

const USAGE: &str = "\
fuzzlite: find inputs that crash Carrysheet's readers or make them blow up

Usage:
  fuzzlite list
  fuzzlite run --target NAME [--seconds N | --runs N] [options]
  fuzzlite repro --target NAME FILE...

Run options:
  --seconds N           how long to fuzz (default 60)
  --runs N              stop after N cases instead
  --jobs N              cases at a time (default 1, at most half the cores)
  --seed N              random seed, to repeat a run (default: from the clock)
  --out DIR             where findings go (default ~/.cache/carrysheet-fuzz;
                        never inside the repository)
  --seeds PATH          more seed files or folders (repeatable)
  --root DIR            the repository, for the built-in seed folders
  --max-len BYTES       largest mutant, and the largest input a slow or
                        big case counts as amplification for (default 1048576)
  --no-minimize         keep findings as found
  --minimize-runs N     cases spent minimising one finding (default 2000)
  --minimize-seconds N  time spent minimising one finding (default 120)
  --max-resource-findings N
                        time and memory findings kept (default 5; panics
                        and aborts are never capped; old name --max-per-kind)

Limits (run and repro):
  --timeout-ms N        wall-clock limit per case (default 5000)
  --memory-mib N        memory limit per case, Linux only (default: half the
                        machine's memory split over the jobs, at most 1024)
  --amp-ms N            a case this slow is an amplification (default 2000)
  --amp-mib N           ...or one using this much memory, Linux only
                        (default 256)

Findings print as one JSON line each on stdout; progress goes to stderr.
Exit code: 0 nothing found, 1 findings or failure, 2 usage.
Build with --release: debug builds are slow enough to look like blow-ups.";

const VALUE_FLAGS: &[&str] = &[
    "target",
    "seconds",
    "runs",
    "jobs",
    "seed",
    "out",
    "seeds",
    "root",
    "max-len",
    "minimize-runs",
    "minimize-seconds",
    "max-resource-findings",
    "max-per-kind",
    "timeout-ms",
    "memory-mib",
    "amp-ms",
    "amp-mib",
];
const SWITCHES: &[&str] = &["no-minimize"];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    let result = match args.first().map(String::as_str) {
        Some(CHILD_COMMAND) => {
            return match rest.first().and_then(|n| targets::find(n)) {
                Some(t) => child::child_main(t, rest.get(1).and_then(|v| v.parse().ok())),
                None => ExitCode::from(2),
            };
        }
        Some("list") => list(),
        Some("run") => Flags::parse(rest).and_then(|f| run(&f)),
        Some("repro") => Flags::parse(rest).and_then(|f| repro(&f)),
        Some("help" | "--help" | "-h") => {
            say!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(Usage(format!("unknown command `{other}`"))),
        None => Err(Usage("no command given".into())),
    };
    match result {
        Ok(code) => code,
        Err(Usage(msg)) => {
            say!("fuzzlite: {msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// A usage error: exit code 2 with the usage text.
struct Usage(String);

fn fail(msg: String) -> Result<ExitCode, Usage> {
    say!("fuzzlite: {msg}");
    Ok(ExitCode::from(1))
}

struct Flags {
    values: HashMap<String, Vec<String>>,
    switches: Vec<String>,
    files: Vec<String>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Flags, Usage> {
        let mut f = Flags {
            values: HashMap::new(),
            switches: Vec::new(),
            files: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let Some(flag) = arg.strip_prefix("--") else {
                f.files.push(arg.clone());
                continue;
            };
            let (name, inline) = match flag.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (flag, None),
            };
            if SWITCHES.contains(&name) {
                f.switches.push(name.to_string());
            } else if VALUE_FLAGS.contains(&name) {
                let value = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .cloned()
                        .ok_or_else(|| Usage(format!("--{name} needs a value")))?,
                };
                f.values.entry(name.to_string()).or_default().push(value);
            } else {
                return Err(Usage(format!("unknown option --{name}")));
            }
        }
        Ok(f)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name)?.last().map(String::as_str)
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.values.get(name).cloned().unwrap_or_default()
    }

    fn num(&self, name: &str) -> Result<Option<u64>, Usage> {
        self.get(name)
            .map(|v| {
                v.parse()
                    .map_err(|_| Usage(format!("--{name} takes a whole number, not `{v}`")))
            })
            .transpose()
    }

    fn target(&self) -> Result<&'static targets::Target, Usage> {
        let name = self
            .get("target")
            .ok_or_else(|| Usage("--target is required (see `fuzzlite list`)".into()))?;
        targets::find(name)
            .ok_or_else(|| Usage(format!("no target `{name}` (see `fuzzlite list`)")))
    }

    fn max_len(&self) -> Result<usize, Usage> {
        Ok(self.num("max-len")?.unwrap_or(1 << 20) as usize)
    }

    /// `--max-resource-findings`, or its old name `--max-per-kind`.
    fn max_resource_findings(&self) -> Result<usize, Usage> {
        let v = match self.num("max-resource-findings")? {
            Some(v) => Some(v),
            None => self.num("max-per-kind")?,
        };
        Ok(v.unwrap_or(5) as usize)
    }

    fn limits(&self, jobs: usize) -> Result<Limits, Usage> {
        let mut l = Limits {
            memory_bytes: child::default_memory_bytes(jobs),
            // A slow case only counts as an amplification if it is no bigger
            // than the largest mutant the run makes.
            amp_input: self.max_len()?,
            ..Limits::default()
        };
        if let Some(v) = self.num("timeout-ms")? {
            l.timeout = Duration::from_millis(v.max(1));
        }
        if let Some(v) = self.num("memory-mib")? {
            l.memory_bytes = v.max(16) << 20;
        }
        if let Some(v) = self.num("amp-ms")? {
            l.amp_time = Duration::from_millis(v);
        }
        if let Some(v) = self.num("amp-mib")? {
            l.amp_bytes = v << 20;
        }
        Ok(l)
    }
}

fn list() -> Result<ExitCode, Usage> {
    for t in targets::all().into_iter().filter(|t| !t.hidden) {
        stdout_line(&format!("{:<16}{}", t.name, t.about));
    }
    Ok(ExitCode::SUCCESS)
}

fn exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot find the fuzzlite binary: {e}"))
}

fn run(f: &Flags) -> Result<ExitCode, Usage> {
    let target = f.target()?;
    let runs = f.num("runs")?;
    let seconds = f.num("seconds")?.unwrap_or(if runs.is_some() {
        u64::from(u32::MAX)
    } else {
        60
    });
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let half = (cores / 2).max(1);
    let mut jobs = f.num("jobs")?.unwrap_or(1).max(1) as usize;
    if jobs > half {
        say!("fuzzlite: the machine is shared; using {half} jobs, half of {cores} cores");
        jobs = half;
    }
    let limits = f.limits(jobs)?;
    let rng_seed = match f.num("seed")? {
        Some(s) => s,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64),
    };
    let root = f
        .get("root")
        .map_or_else(seeds::default_root, PathBuf::from);
    let out = match f
        .get("out")
        .map(PathBuf::from)
        .or_else(paths::default_out_dir)
    {
        Some(o) => o,
        None => return Err(Usage("no cache folder found; give --out".into())),
    };
    let out = match paths::prepare_out_dir(&out, &root) {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    let extra: Vec<PathBuf> = f.all("seeds").into_iter().map(PathBuf::from).collect();
    if extra.is_empty() && !root.join("crates").is_dir() {
        return fail(format!(
            "the repository folder {} is not there; fuzzlite reads its seed files from the tests/data folders in it. Pass --root <repository> or --seeds <folder>",
            display_path(&root)
        ));
    }
    let seed_list = match seeds::load(target, &root, &extra) {
        Ok(s) => s,
        Err(e) => return fail(format!("cannot read the seed files: {e}")),
    };
    let minimize = (!f.switches.iter().any(|s| s == "no-minimize")).then(|| Budget {
        runs: f.num("minimize-runs").ok().flatten().unwrap_or(2000) as usize,
        time: Duration::from_secs(f.num("minimize-seconds").ok().flatten().unwrap_or(120)),
    });
    let exe = match exe() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    let opts = Options {
        target,
        exe,
        seeds: seed_list,
        time: Duration::from_secs(seconds),
        runs,
        jobs,
        rng_seed,
        limits,
        out: out.clone(),
        minimize,
        max_len: f.max_len()?,
        max_resource_findings: f.max_resource_findings()?,
    };
    say!(
        "fuzzlite: target {}, {} seeds, seed {rng_seed}, {jobs} job(s), findings in {}",
        target.name,
        opts.seeds.len(),
        display_path(&out)
    );
    memory_note();
    let summary = match campaign::campaign(&opts) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let mut by_kind: std::collections::BTreeMap<&str, usize> = Default::default();
    for finding in &summary.findings {
        *by_kind.entry(finding.kind.as_str()).or_default() += 1;
    }
    say!(
        "fuzzlite: done, {} cases, {} findings {:?}, {} repeats, {} dropped at the cap ({} cases), {} flaky, {} panics caught inside cfb",
        summary.cases,
        summary.findings.len(),
        by_kind,
        summary.repeats,
        summary.dropped_signatures,
        summary.dropped_at_cap,
        summary.flaky,
        summary.caught_cfb
    );
    if let Some((ms, len, family)) = &summary.slowest {
        let memory = if child::MEMORY_MEASURED {
            format!("most memory {} MiB", summary.max_peak_kib.div_ceil(1024))
        } else {
            "memory not measured".to_string()
        };
        say!("fuzzlite: slowest passing case {ms} ms ({len} bytes, {family}); {memory}");
    }
    Ok(if summary.findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Says, where it is so, that memory is neither limited nor measured, so
/// a quiet run is not read as "no memory blow-ups".
fn memory_note() {
    if !child::MEMORY_MEASURED {
        say!(
            "fuzzlite: memory is not limited or measured on this system (Linux only), so --memory-mib and --amp-mib do nothing and only time blow-ups are found"
        );
    }
}

fn repro(f: &Flags) -> Result<ExitCode, Usage> {
    let target = f.target()?;
    let limits = f.limits(1)?;
    if f.files.is_empty() {
        return Err(Usage("repro needs at least one input file".into()));
    }
    memory_note();
    let exe = match exe() {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    if let Err(e) = check_child(&exe, &limits) {
        return fail(e);
    }
    let mut bad = false;
    for file in &f.files {
        let data = match std::fs::read(file) {
            Ok(d) => d,
            Err(e) => return fail(format!("cannot read {file}: {e}")),
        };
        let o = match run_case(&exe, target.name, &data, &limits) {
            Ok(o) => o,
            Err(e) => return fail(format!("cannot run the child: {e}")),
        };
        bad |= o.kind != Kind::Ok;
        let detail = match &o.location {
            Some(loc) => format!("{loc}: {}", o.detail),
            None => o.detail.clone(),
        };
        stdout_line(
            &JsonLine::new()
                .str("file", file)
                .str("target", target.name)
                .str("kind", o.kind.as_str())
                .str("detail", &detail)
                .num("input_bytes", data.len() as u64)
                .num("millis", o.millis)
                .opt_num("peak_mib", o.peak_kib.map(|k| k.div_ceil(1024)))
                .finish(),
        );
    }
    Ok(if bad {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
