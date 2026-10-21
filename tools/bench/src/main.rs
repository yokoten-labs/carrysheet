//! `bench`: times Carrysheet's readers and writers on fixed files and
//! catches slowdowns and memory growth against `tools/bench/baseline.json`.
//!
//! See `tools/bench/README.md`.

mod baseline;
mod cases;
mod generate;
mod json;
mod runner;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use baseline::{LIMIT, RERUNS, Verdict};
use cases::{Case, all};
use json::{opt, quote};
use runner::Measured;

const USAGE: &str = "\
Usage: bench [run|compare|update|list] [options]

  run       Time every case and print the numbers (the default).
  compare   Time every case and fail (exit 1) when one is more than 10%
            slower (median against median, scaled by a reference
            workload timed alongside, so a busier machine does not
            count) or uses more than 10% more peak memory than the
            baseline, after 3 reruns to rule out noise.
  update    Time every case and rewrite the baseline. Explain the change
            in the commit message.
  list      List the cases.

Options:
  --json             JSON on stdout instead of lines for people.
  --runs N           Timed runs per case (default 5); the median is reported.
  --filter TEXT      Only cases whose name contains TEXT.
  --baseline PATH    The baseline file (default tools/bench/baseline.json).
  --inputs DIR       Where generated inputs go (default target/bench-inputs).

Build with --release: cargo run --release -p carrysheet-bench -- compare
";

/// Printed with every run: the numbers come from a shared machine.
const NOISE: &str = "note: timings come from a shared machine and are noisy. Times are scaled by a reference workload (\"machine\" is how much slower the machine is than for the baseline), and only changes over 10% that survive 3 reruns count.";

struct Args {
    command: String,
    json: bool,
    runs: u32,
    filter: Option<String>,
    baseline: Option<PathBuf>,
    inputs: Option<PathBuf>,
    case: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        command: "run".into(),
        json: false,
        runs: 5,
        filter: None,
        baseline: None,
        inputs: None,
        case: None,
    };
    let mut it = std::env::args().skip(1);
    let mut positional = Vec::new();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--json" => args.json = true,
            "--runs" => {
                args.runs = value("--runs")?
                    .parse()
                    .ok()
                    .filter(|n| (1..=1000).contains(n))
                    .ok_or("--runs takes a number from 1 to 1000")?;
            }
            "--filter" => args.filter = Some(value("--filter")?),
            "--baseline" => args.baseline = Some(value("--baseline")?.into()),
            "--inputs" => args.inputs = Some(value("--inputs")?.into()),
            "-h" | "--help" => args.command = "help".into(),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ => positional.push(a),
        }
    }
    let mut pos = positional.into_iter();
    if let Some(c) = pos.next()
        && args.command != "help"
    {
        args.command = c;
    }
    args.case = pos.next();
    if pos.next().is_some() || (args.case.is_some() && args.command != "child") {
        return Err("too many arguments".into());
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => return usage_error(&e),
    };
    let code = match args.command.as_str() {
        "help" => {
            out(USAGE.trim_end());
            0
        }
        "reference" => runner::reference_child(),
        "child" => match &args.case {
            Some(name) => {
                runner::child(name, args.runs, &runner::inputs_dir(args.inputs.as_deref()))
            }
            None => return usage_error("child needs a case name"),
        },
        "list" => list(&args),
        "run" | "compare" | "update" => match measure_and_report(&args) {
            Ok(code) => code,
            Err(e) => {
                note(&format!("bench: {e}"));
                1
            }
        },
        other => return usage_error(&format!("unknown command {other}")),
    };
    ExitCode::from(code as u8)
}

fn usage_error(e: &str) -> ExitCode {
    note(&format!("bench: {e}\n\n{USAGE}"));
    ExitCode::from(2)
}

/// The one place the bench writes to stdout: results only, and with
/// `--json` JSON only. (The workspace forbids `std::io::stdout` elsewhere
/// because of MCP; the bench has no MCP side.)
#[allow(clippy::disallowed_methods)]
pub fn out(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{}", scrub(line));
    let _ = stdout.flush();
}

/// Writes one message line to stderr.
pub fn note(line: &str) {
    eprintln!("{}", scrub(line));
}

/// Shows the home folder as `~`, so output never names the user.
fn scrub(line: &str) -> String {
    match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        Ok(home) if home.len() > 1 => line.replace(&home, "~"),
        _ => line.to_string(),
    }
}

fn selected(args: &Args) -> Vec<Case> {
    all()
        .into_iter()
        .filter(|c| args.filter.as_deref().is_none_or(|f| c.name.contains(f)))
        .collect()
}

fn list(args: &Args) -> i32 {
    use cases::Operation::{Open, Recalc, Save};
    let cases = selected(args);
    // Per file, which of the operations people feel have no case yet.
    let mut files: Vec<&str> = Vec::new();
    for c in &cases {
        if !files.contains(&c.file()) {
            files.push(c.file());
        }
    }
    let missing = |file: &str| -> Vec<&'static str> {
        [Open, Save, Recalc]
            .into_iter()
            .filter(|op| !cases.iter().any(|c| c.file() == file && c.op == *op))
            .map(|op| op.as_str())
            .collect()
    };
    if args.json {
        let items: Vec<String> = cases
            .iter()
            .map(|c| {
                format!(
                    "{{\"name\": {}, \"file\": {}, \"operation\": {}, \"about\": {}}}",
                    quote(c.name),
                    quote(c.file()),
                    quote(c.op.as_str()),
                    quote(c.about)
                )
            })
            .collect();
        let gaps: Vec<String> = files
            .iter()
            .map(|f| {
                let ops: Vec<String> = missing(f).iter().map(|o| quote(o)).collect();
                format!(
                    "{{\"file\": {}, \"missing\": [{}]}}",
                    quote(f),
                    ops.join(", ")
                )
            })
            .collect();
        out(&format!(
            "{{\"cases\": [{}], \"files\": [{}]}}",
            items.join(", "),
            gaps.join(", ")
        ));
    } else {
        for c in &cases {
            out(&format!("{:<34} {}", c.name, c.about));
        }
        out("");
        out("Not measured yet (no writer, or the kernel is not merged):");
        for f in &files {
            let m = missing(f);
            if !m.is_empty() {
                out(&format!("  {:<32} {}", f, m.join(", ")));
            }
        }
    }
    0
}

fn baseline_display(args: &Args) -> String {
    match &args.baseline {
        Some(p) => p.display().to_string(),
        None => "tools/bench/baseline.json".into(),
    }
}

fn measure_and_report(args: &Args) -> Result<i32, String> {
    let cases = selected(args);
    if cases.is_empty() {
        return Err(format!(
            "no case matches --filter {}; `bench list` shows the names",
            args.filter.as_deref().unwrap_or("")
        ));
    }
    let dir = runner::inputs_dir(args.inputs.as_deref());
    for g in cases::generated() {
        if !cases
            .iter()
            .any(|c| matches!(c.input, cases::Input::Generated(h) if std::ptr::eq(h, g)))
        {
            continue;
        }
        if !dir.join(g.file_name()).is_file() {
            note(&format!("bench: generating {} ...", g.file_name()));
        }
        runner::ensure_generated(g, &dir)
            .map_err(|e| format!("cannot write the input {}: {e}", g.file_name()))?;
    }
    let base_path = args.baseline.clone().unwrap_or_else(baseline::default_path);
    let base = match args.command.as_str() {
        "compare" => Some(baseline::load(&base_path)?),
        "update" if args.filter.is_some() => Some(baseline::load(&base_path)?),
        _ => None,
    };
    let machine = runner::machine();
    if let Some(b) = &base
        && args.command == "compare"
        && b.machine != machine
    {
        eprintln!(
            "bench: warning: the baseline was measured on another machine ({}); this is {machine}. Times will differ for that reason alone.",
            b.machine
        );
    }

    if !args.json {
        out(NOISE);
    }
    if let Some(b) = &base
        && args.command == "compare"
        && b.runs != args.runs
    {
        note(&format!(
            "bench: warning: the baseline took the median of {} runs per case, this run {}.",
            b.runs, args.runs
        ));
    }
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for case in &cases {
        // The baseline pools RERUNS measurements, each in its own process,
        // so it records the typical time rather than one process's luck.
        let times = if args.command == "update" { RERUNS } else { 1 };
        let m = match (0..times)
            .map(|_| runner::run_case(case, args.runs, &dir))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(ms) => baseline::pooled(&ms),
            Err(e) => {
                if !args.json {
                    out(&format!("{:<34} FAILED: {e}", case.name));
                }
                errors.push((case.name, e));
                continue;
            }
        };
        let mut row = Row {
            m,
            base: None,
            verdict: None,
            reruns: 0,
        };
        if args.command == "compare"
            && let Some(entry) = base.as_ref().and_then(|b| b.get(case.name))
        {
            let mut v = baseline::judge(&row.m, entry);
            if v.slower || v.hungrier {
                // Rule out noise: rerun, and keep the best.
                let mut runs = vec![row.m.clone()];
                for _ in 0..RERUNS {
                    runs.push(runner::run_case(case, args.runs, &dir)?);
                }
                row.m = baseline::best(&runs);
                row.reruns = RERUNS;
                v = baseline::judge(&row.m, entry);
            }
            if v.failed() {
                failures.push(failure_text(case.name, &row.m, entry, &v, row.reruns));
            }
            row.base = Some(entry.clone());
            row.verdict = Some(v);
        }
        if !args.json {
            out(&human_line(&row));
        }
        results.push(row.m.clone());
        rows.push(row);
    }

    let mut missing = Vec::new();
    let mut new_cases = Vec::new();
    if args.command == "compare"
        && let Some(b) = &base
    {
        if args.filter.is_none() {
            for e in &b.entries {
                if !cases.iter().any(|c| c.name == e.name) {
                    missing.push(e.name.clone());
                }
            }
        }
        for r in &rows {
            if r.base.is_none() {
                new_cases.push(r.m.name.clone());
            }
        }
    }

    let failed = !errors.is_empty() || !failures.is_empty() || !missing.is_empty();
    if args.command == "update" && errors.is_empty() {
        let mut all_results = results.clone();
        if let Some(b) = &base {
            // --filter: keep the cases not rerun, in the order of `all()`.
            all_results = all()
                .iter()
                .filter_map(|c| {
                    results
                        .iter()
                        .find(|m| m.name == c.name)
                        .cloned()
                        .or_else(|| b.get(c.name).map(baseline::Entry::as_measured))
                })
                .collect();
        }
        std::fs::write(
            &base_path,
            baseline::render(&machine, args.runs, &all_results),
        )
        .map_err(|e| format!("cannot write {}: {e}", baseline_display(args)))?;
    }

    if args.json {
        let cases_json: Vec<String> = rows.iter().map(row_json).collect();
        let errs: Vec<String> = errors
            .iter()
            .map(|(n, e)| format!("{{\"name\": {}, \"error\": {}}}", quote(n), quote(e)))
            .collect();
        let names = |v: &[String]| v.iter().map(|n| quote(n)).collect::<Vec<_>>().join(", ");
        out(&format!(
            "{{\"command\": {}, \"ok\": {}, \"note\": {}, \"machine\": {}, \"runs\": {}, \"limit\": {LIMIT}, \"cases\": [{}], \"errors\": [{}], \"missing\": [{}], \"new\": [{}]}}",
            quote(&args.command),
            !failed,
            quote(NOISE),
            quote(&machine),
            args.runs,
            cases_json.join(", "),
            errs.join(", "),
            names(&missing),
            names(&new_cases),
        ));
    } else {
        for n in &new_cases {
            out(&format!(
                "note: {n} has no baseline yet; run `bench update` to add it."
            ));
        }
        if args.command == "compare" {
            if failures.is_empty() && missing.is_empty() && errors.is_empty() {
                out(&format!(
                    "ok: no case is more than {:.0}% slower or hungrier than {}.",
                    LIMIT * 100.0,
                    baseline_display(args)
                ));
            } else {
                out("");
                if !failures.is_empty() {
                    out(&format!(
                        "FAILED: {} case{} worse than {} by more than {:.0}%:",
                        failures.len(),
                        if failures.len() == 1 { " is" } else { "s are" },
                        baseline_display(args),
                        LIMIT * 100.0
                    ));
                    for f in &failures {
                        out(&format!("  {f}"));
                    }
                }
                for n in &missing {
                    out(&format!(
                        "FAILED: {n} is in the baseline but no longer a case."
                    ));
                }
                out(
                    "If the change is expected, run `bench update` and say in the commit message what changed and why it is accepted.",
                );
            }
        }
        if args.command == "update" && errors.is_empty() {
            out(&format!(
                "Wrote {}. Commit it, and explain in the commit message why the numbers changed: which cases moved, by how much, and why that is accepted.",
                baseline_display(args)
            ));
        }
        if !errors.is_empty() {
            out(&format!(
                "FAILED: {} case{} could not run (see above).",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" }
            ));
        }
    }
    Ok(i32::from(failed))
}

struct Row {
    m: Measured,
    base: Option<baseline::Entry>,
    verdict: Option<Verdict>,
    reruns: u32,
}

fn row_json(r: &Row) -> String {
    let mut s = baseline::case_json(&r.m);
    s.pop(); // the closing brace
    if let (Some(b), Some(v)) = (&r.base, &r.verdict) {
        s.push_str(&format!(
            ", \"baseline_median_ns\": {}, \"baseline_peak_kib\": {}, \"time_change\": {:.4}, \"memory_change\": {}, \"machine_change\": {}, \"reruns\": {}, \"slower\": {}, \"hungrier\": {}, \"input_changed\": {}, \"check_changed\": {}",
            b.median_ns,
            opt(b.peak_kib),
            v.time,
            opt(v.memory.map(|m| format!("{m:.4}"))),
            opt(v.machine.map(|m| format!("{m:.4}"))),
            r.reruns,
            v.slower,
            v.hungrier,
            v.input_changed,
            v.check_changed
        ));
    }
    s.push('}');
    s
}

fn human_line(r: &Row) -> String {
    let m = &r.m;
    let mut line = format!(
        "{:<34} {:>10} ±{:>4.1}%  peak {:>10}",
        m.name,
        time(m.median_ns()),
        m.spread() * 50.0,
        m.peak_kib.map_or("n/a".to_string(), memory)
    );
    if let Some(v) = &r.verdict {
        line.push_str(&format!(
            "   time {:>+6.1}%  peak {:>7}  machine {:>7}",
            v.time * 100.0,
            v.memory
                .map_or("n/a".to_string(), |x| format!("{:+.1}%", x * 100.0)),
            v.machine
                .map_or("n/a".to_string(), |x| format!("{:+.0}%", x * 100.0))
        ));
        let mut tags = Vec::new();
        if v.slower {
            tags.push("SLOWER");
        }
        if v.hungrier {
            tags.push("HUNGRIER");
        }
        if v.input_changed {
            tags.push("INPUT CHANGED");
        }
        if v.check_changed {
            tags.push("result changed");
        }
        if r.reruns > 0 {
            tags.push(if v.slower || v.hungrier {
                "after 3 reruns"
            } else {
                "noise, fine after 3 reruns"
            });
        }
        if !tags.is_empty() {
            line.push_str(&format!("  [{}]", tags.join(", ")));
        }
    }
    line
}

fn failure_text(name: &str, m: &Measured, b: &baseline::Entry, v: &Verdict, reruns: u32) -> String {
    let mut parts = Vec::new();
    if v.slower {
        parts.push(format!(
            "{:.1}% slower (median {} -> {}, {} at the baseline's machine speed)",
            v.time * 100.0,
            time(b.median_ns),
            time(m.median_ns()),
            time(baseline::normalized(m, b.ref_ns))
        ));
    }
    if v.hungrier {
        parts.push(format!(
            "{:.1}% more peak memory ({} -> {})",
            v.memory.unwrap_or(0.0) * 100.0,
            memory(b.peak_kib.unwrap_or(0)),
            memory(m.peak_kib.unwrap_or(0))
        ));
    }
    if v.input_changed {
        parts.push("its input is not the one the baseline measured (a generator changed?)".into());
    }
    let tail = if reruns > 0 {
        format!(", best of {reruns} reruns")
    } else {
        String::new()
    };
    format!("{name}: {}{tail}", parts.join("; "))
}

fn time(ns: u64) -> String {
    let ns = ns as f64;
    if ns < 1e3 {
        format!("{ns:.0} ns")
    } else if ns < 1e6 {
        format!("{:.1} µs", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.1} ms", ns / 1e6)
    } else {
        format!("{:.2} s", ns / 1e9)
    }
}

fn memory(kib: u64) -> String {
    format!("{:.1} MiB", kib as f64 / 1024.0)
}
