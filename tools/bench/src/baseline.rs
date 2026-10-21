//! The committed baseline (`tools/bench/baseline.json`) and the comparison
//! against it.

use std::path::{Path, PathBuf};

use crate::json::{self, Json, opt, quote};
use crate::runner::Measured;

/// A case more than this much slower, or with this much more peak memory,
/// fails `compare`.
pub const LIMIT: f64 = 0.10;

/// Peak memory differences below this are allocator noise, not growth.
pub const MEMORY_FLOOR_KIB: u64 = 256;

/// Rounding slack, so exactly 10% is not over the line.
const EPSILON: f64 = 1e-9;

/// Times a suspect case is rerun before it counts as a regression, and
/// times `update` measures each case (in separate processes) to pool.
pub const RERUNS: u32 = 3;

pub fn default_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("baseline.json")
}

/// One case as the baseline recorded it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub median_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub peak_kib: Option<u64>,
    pub check: u64,
    /// The reference workload's time when the baseline was measured.
    pub ref_ns: u64,
    pub input_bytes: u64,
    pub input_hash: Option<String>,
}

impl Entry {
    /// The entry as a measurement, so `update --filter` can write back the
    /// cases it did not rerun.
    pub fn as_measured(&self) -> Measured {
        Measured {
            name: self.name.clone(),
            samples_ns: vec![self.min_ns, self.median_ns, self.max_ns],
            peak_kib: self.peak_kib,
            check: self.check,
            ref_ns: self.ref_ns,
            input_bytes: self.input_bytes,
            input_hash: self.input_hash.clone(),
        }
    }
}

pub struct Baseline {
    pub machine: String,
    pub runs: u32,
    pub entries: Vec<Entry>,
}

impl Baseline {
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

pub fn load(path: &Path) -> Result<Baseline, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "cannot read the baseline {}: {e}. Run `bench update` to write one.",
            path.display()
        )
    })?;
    let bad = |what: &str| format!("the baseline {} is damaged: {what}", path.display());
    let v = json::parse(&text).map_err(|e| bad(&e))?;
    let mut entries = Vec::new();
    for c in v
        .get("cases")
        .and_then(Json::as_arr)
        .ok_or_else(|| bad("no \"cases\" list"))?
    {
        let num = |k: &str| c.get(k).and_then(Json::as_u64);
        entries.push(Entry {
            name: c
                .get("name")
                .and_then(Json::as_str)
                .ok_or_else(|| bad("a case without a name"))?
                .to_string(),
            median_ns: num("median_ns").ok_or_else(|| bad("a case without median_ns"))?,
            min_ns: num("min_ns").unwrap_or(0),
            max_ns: num("max_ns").unwrap_or(0),
            peak_kib: num("peak_kib"),
            check: num("check").unwrap_or(0),
            ref_ns: num("ref_ns").unwrap_or(0),
            input_bytes: num("input_bytes").unwrap_or(0),
            input_hash: c
                .get("input_hash")
                .and_then(Json::as_str)
                .map(str::to_string),
        });
    }
    Ok(Baseline {
        machine: v
            .get("machine")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string(),
        runs: v.get("runs").and_then(Json::as_u64).unwrap_or(0) as u32,
        entries,
    })
}

/// The baseline file's text: one case per line, so a diff shows what moved.
pub fn render(machine: &str, runs: u32, results: &[Measured]) -> String {
    let mut out = String::from("{\n");
    out.push_str("  \"format\": 1,\n");
    out.push_str(&format!(
        "  \"note\": {},\n",
        quote(
            "Written by `bench update`. Explain every change to this file in the commit message."
        )
    ));
    out.push_str(&format!("  \"machine\": {},\n", quote(machine)));
    out.push_str(&format!("  \"runs\": {runs},\n"));
    out.push_str("  \"cases\": [\n");
    for (i, m) in results.iter().enumerate() {
        out.push_str(&format!(
            "    {}{}\n",
            case_json(m),
            if i + 1 < results.len() { "," } else { "" }
        ));
    }
    out.push_str("  ]\n}\n");
    out
}

/// One measured case as a JSON object on one line.
pub fn case_json(m: &Measured) -> String {
    format!(
        "{{\"name\": {}, \"file\": {}, \"operation\": {}, \"median_ns\": {}, \"min_ns\": {}, \"max_ns\": {}, \"peak_kib\": {}, \"check\": {}, \"ref_ns\": {}, \"input_bytes\": {}, \"input_hash\": {}}}",
        quote(&m.name),
        quote(m.name.split_once('/').map_or("", |(f, _)| f)),
        quote(m.name.split_once('/').map_or("", |(_, o)| o)),
        m.median_ns(),
        m.min_ns(),
        m.max_ns(),
        opt(m.peak_kib),
        m.check,
        m.ref_ns,
        m.input_bytes,
        m.input_hash.as_deref().map_or("null".to_string(), quote),
    )
}

/// How one case compares with its baseline entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// Fractional change of the median time, scaled to the baseline's
    /// machine speed (0.12 = 12% slower).
    pub time: f64,
    /// Fractional change of the machine's speed (the reference workload),
    /// when both sides timed it.
    pub machine: Option<f64>,
    /// Fractional change of peak memory, when both sides have it.
    pub memory: Option<f64>,
    pub slower: bool,
    pub hungrier: bool,
    /// The input differs from the one the baseline measured.
    pub input_changed: bool,
    /// The check value differs: the operation computes something else now.
    pub check_changed: bool,
}

impl Verdict {
    pub fn failed(&self) -> bool {
        self.slower || self.hungrier || self.input_changed
    }
}

pub fn judge(m: &Measured, base: &Entry) -> Verdict {
    let time = ratio(normalized(m, base.ref_ns), base.median_ns);
    let memory = match (m.peak_kib, base.peak_kib) {
        (Some(now), Some(then)) => Some(ratio(now, then)),
        _ => None,
    };
    let hungrier = match (m.peak_kib, base.peak_kib) {
        (Some(now), Some(then)) => {
            now > then + MEMORY_FLOOR_KIB && ratio(now, then) > LIMIT + EPSILON
        }
        _ => false,
    };
    Verdict {
        time,
        machine: machine_change(m, base.ref_ns),
        memory,
        slower: time > LIMIT + EPSILON,
        hungrier,
        input_changed: matches!((&m.input_hash, &base.input_hash), (Some(a), Some(b)) if a != b),
        check_changed: m.check != base.check,
    }
}

/// The median time scaled to the machine speed of the baseline: a machine
/// that is busier or warmer now than then (the reference workload took
/// longer) does not make the code look slower.
pub fn normalized(m: &Measured, base_ref_ns: u64) -> u64 {
    if base_ref_ns > 0 && m.ref_ns > 0 {
        (m.median_ns() as f64 * base_ref_ns as f64 / m.ref_ns as f64) as u64
    } else {
        m.median_ns()
    }
}

/// How much slower the machine is now than when the baseline was taken,
/// as a fraction, or `None` without reference times.
pub fn machine_change(m: &Measured, base_ref_ns: u64) -> Option<f64> {
    (base_ref_ns > 0 && m.ref_ns > 0).then(|| ratio(m.ref_ns, base_ref_ns))
}

fn ratio(now: u64, then: u64) -> f64 {
    if then == 0 {
        0.0
    } else {
        now as f64 / then as f64 - 1.0
    }
}

/// The best of several measurements of one case: the one with the lowest
/// median relative to the reference workload, and the lowest peak memory. Noise on a
/// shared machine only ever adds, so the best is the fairest.
fn relative(m: &Measured) -> f64 {
    m.median_ns() as f64 / m.ref_ns.max(1) as f64
}

pub fn best(runs: &[Measured]) -> Measured {
    let mut best = runs[0].clone();
    for m in &runs[1..] {
        if relative(m) < relative(&best) {
            best.samples_ns = m.samples_ns.clone();
            best.ref_ns = m.ref_ns;
        }
        best.peak_kib = match (best.peak_kib, m.peak_kib) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    best
}

/// Several measurements of one case pooled into one, for the baseline:
/// all timed runs (so the median is the typical time over several
/// processes, not a lucky one) and the lowest peak memory.
pub fn pooled(runs: &[Measured]) -> Measured {
    let mut all = runs[0].clone();
    for m in &runs[1..] {
        all.samples_ns.extend_from_slice(&m.samples_ns);
        all.ref_ns += m.ref_ns;
        all.peak_kib = match (all.peak_kib, m.peak_kib) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    all.ref_ns /= runs.len() as u64;
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured(ns: u64, kib: u64) -> Measured {
        Measured {
            name: "a/b".into(),
            samples_ns: vec![ns],
            peak_kib: Some(kib),
            check: 7,
            ref_ns: 1000,
            input_bytes: 3,
            input_hash: Some("00ff".into()),
        }
    }

    fn entry(ns: u64, kib: u64) -> Entry {
        Entry {
            name: "a/b".into(),
            median_ns: ns,
            min_ns: ns,
            max_ns: ns,
            peak_kib: Some(kib),
            check: 7,
            ref_ns: 1000,
            input_bytes: 3,
            input_hash: Some("00ff".into()),
        }
    }

    #[test]
    fn ten_percent_is_the_line() {
        assert!(!judge(&measured(110, 10_000), &entry(100, 10_000)).failed());
        assert!(judge(&measured(111, 10_000), &entry(100, 10_000)).slower);
        assert!(judge(&measured(100, 11_100), &entry(100, 10_000)).hungrier);
        assert!(!judge(&measured(90, 9_000), &entry(100, 10_000)).failed());
    }

    #[test]
    fn a_slower_machine_is_not_slower_code() {
        // 30% slower, but the reference workload was 30% slower too.
        let mut m = measured(130, 10_000);
        m.ref_ns = 1300;
        assert!(!judge(&m, &entry(100, 10_000)).slower);
        m.ref_ns = 1000;
        assert!(judge(&m, &entry(100, 10_000)).slower);
    }

    #[test]
    fn small_memory_changes_are_noise() {
        // +20%, but only 200 KiB.
        assert!(!judge(&measured(100, 1_200), &entry(100, 1_000)).hungrier);
    }

    #[test]
    fn changed_input_fails() {
        let mut m = measured(100, 1_000);
        m.input_hash = Some("beef".into());
        assert!(judge(&m, &entry(100, 1_000)).input_changed);
        m.input_hash = None; // salted
        assert!(!judge(&m, &entry(100, 1_000)).input_changed);
    }

    #[test]
    fn render_loads_back() {
        let text = render("test machine", 5, &[measured(1234, 5678)]);
        let dir = std::env::temp_dir().join(format!("carrysheet-bench-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("baseline.json");
        std::fs::write(&path, text).unwrap();
        let b = load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((b.machine.as_str(), b.runs), ("test machine", 5));
        assert_eq!(b.entries, [entry(1234, 5678)]);
    }

    #[test]
    fn pooled_keeps_every_run() {
        let p = pooled(&[measured(300, 900), measured(200, 1000), measured(250, 800)]);
        assert_eq!(
            (p.median_ns(), p.min_ns(), p.peak_kib),
            (250, 200, Some(800))
        );
    }

    #[test]
    fn best_takes_the_lowest() {
        let b = best(&[measured(300, 900), measured(200, 1000), measured(250, 800)]);
        assert_eq!(
            (b.median_ns(), b.min_ns(), b.peak_kib),
            (200, 200, Some(800))
        );
    }
}
