//! Shared by the open-path tests (record 0010, decision 3): the files and
//! the builders of the readers that left with the cut now test the one open
//! path that is left, GridCraft through this kernel.
//!
//! - [`run_case`] opens one input the way the program does and checks the
//!   contract: it opens, or it fails with a three-part error. A caught panic
//!   is a failure (record 0003), and so is a case slower than the budget
//!   (in a debug build, slower than the hang backstop: see [`BUDGET`]).
//! - [`settle`] fails the test on a failure that no waiver covers, on a
//!   waiver that no case needs any more, and on a waiver without a reason,
//!   an issue and the expiry.
//! - [`Rng`] is a small seeded generator, so every run tries the same inputs.
//! - [`load_parts`] loads the hand-written `.parts` packages as a zip;
//!   [`unpack`] and [`pack`] take a package apart and build it again.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use carrysheet_commands::engine::{Engine, FileFormat, ReadRequest};
use carrysheet_commands::{CommandError, ErrorCode};
use carrysheet_kernel::Kernel;
use carrysheet_model::{SheetRange, format_sheet_name};

/// The budget of one command (`carrysheet_commands::limits::MAX_CALL_SECONDS`).
///
/// A release build fails a case over it. A debug build, which CI's
/// `cargo test` runs, fails a case only past [`HANG`]: debug code is about
/// 18x slower than release here (biff/SharedFormulaTest.xls, 31 KB at
/// [`Depth::Save`]: 3.4 s debug, 0.19 s release on the Mac), and a GitHub
/// macOS runner is about 3.8x slower again, so an honest case would cross
/// 10 s there. A case the kernel stops with its own `limit_exceeded`
/// error is refused, which keeps the contract. Each report prints its
/// slowest case.
pub const BUDGET: Duration = Duration::from_secs(10);

/// The hang backstop of a debug build: see [`BUDGET`].
pub const HANG: Duration = Duration::from_secs(60);

/// What a case may take in this build: [`BUDGET`] in release, [`HANG`] in debug.
pub fn time_limit() -> Duration {
    if cfg!(debug_assertions) { HANG } else { BUDGET }
}

/// The only expiry a waiver may have: every waiver is gone before v0.1.0.
pub const EXPIRY: &str = "v0.1.0";

/// The issue of the waivers here: the review of GridCraft's readers.
pub const ISSUE: u32 = 71;

/// The folder the removed readers' files moved to.
pub fn formats_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/formats")
}

/// The path of a file below `tests/data/formats`, with `/` on every system.
pub fn short_name(path: &Path) -> String {
    let rel = path.strip_prefix(formats_dir()).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every sample file below `tests/data/formats`: notices, licenses, read-me
/// files and dot files are not samples.
pub fn sample_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                walk(&path, out);
            } else if !(name.starts_with('.')
                || name.starts_with("LICENSE")
                || name == "NOTICE"
                || name.ends_with(".md"))
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&formats_dir(), &mut out);
    out.sort();
    out
}

/// xorshift64*: small, fast, and the same on every machine.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Damages a binary file: cut short, bits flipped, or a stretch made
/// random and then cut (often a record header or a length field). The
/// removed BIFF reader's mutation test did the same.
pub fn mutate_binary(rng: &mut Rng, input: &[u8], case: usize) -> (Vec<u8>, String) {
    let mut bytes = input.to_vec();
    let what = match case % 3 {
        0 => {
            let at = rng.below(bytes.len());
            bytes.truncate(at);
            format!("cut at {at}")
        }
        1 => {
            let n = 1 + rng.below(8);
            for _ in 0..n {
                let at = rng.below(bytes.len());
                if let Some(b) = bytes.get_mut(at) {
                    *b ^= 1 << rng.below(8);
                }
            }
            format!("{n} bits flipped")
        }
        _ => {
            let at = rng.below(bytes.len());
            let len = 1 + rng.below(16);
            for i in at..(at + len).min(bytes.len()) {
                bytes[i] = rng.next() as u8;
            }
            let cut = bytes.len() - rng.below(bytes.len() / 4 + 1);
            bytes.truncate(cut);
            format!("{len} bytes at {at} made random, cut to {cut}")
        }
    };
    (bytes, what)
}

/// Bytes that matter to csv or to the text encodings.
const CSV_BYTES: &[u8] = &[
    b'"', b',', b';', b'\t', b'|', b'\r', b'\n', b'=', 0x00, 0x80, 0x81, 0xA1, 0xC3, 0xE4, 0xEF,
    0xFE, 0xFF,
];

/// Bytes that matter to XML.
pub const XML_BYTES: &[u8] = &[
    b'<', b'>', b'/', b'"', b'\'', b'&', b';', b'#', b'=', b':', b' ', b'0', b'9', b'-', 0x00,
    0x80, 0xC3, 0xEF, 0xFF,
];

/// Damages a csv file in small ways, as the removed csv reader's mutation
/// test did: flips, csv bytes put in or taken out, cuts and copies.
pub fn mutate_csv(rng: &mut Rng, input: &[u8]) -> (Vec<u8>, String) {
    mutate_text(rng, input, CSV_BYTES)
}

/// Damages text in small ways: flips, bytes of `alphabet` put in or taken
/// out, cuts and copies.
pub fn mutate_text(rng: &mut Rng, input: &[u8], alphabet: &[u8]) -> (Vec<u8>, String) {
    let mut out = input.to_vec();
    let mut what = Vec::new();
    for _ in 0..=rng.below(4) {
        let at = rng.below(out.len() + 1);
        match rng.below(7) {
            0 if at < out.len() => {
                out[at] ^= 1 << rng.below(8);
                what.push(format!("flip at {at}"));
            }
            1 if at < out.len() => {
                out[at] = alphabet[rng.below(alphabet.len())];
                what.push(format!("set at {at}"));
            }
            2 => {
                out.insert(at, alphabet[rng.below(alphabet.len())]);
                what.push(format!("insert at {at}"));
            }
            3 if at < out.len() => {
                out.remove(at);
                what.push(format!("remove at {at}"));
            }
            4 => {
                out.truncate(at);
                what.push(format!("cut at {at}"));
            }
            5 => {
                let end = (at + rng.below(16)).min(out.len());
                let copy = out[at..end].to_vec();
                let to = rng.below(out.len() + 1);
                out.splice(to..to, copy);
                what.push(format!("copy {at}..{end} to {to}"));
            }
            _ => {
                out.insert(at, rng.next() as u8);
                what.push(format!("random byte at {at}"));
            }
        }
    }
    (out, what.join(", "))
}

thread_local! {
    /// Set while a case runs: its panics are recorded, not printed.
    static IN_CASE: Cell<bool> = const { Cell::new(false) };
    /// Where each panic in the running case happened.
    static PANICS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Replaces the panic hook once per test program: a panic inside a case is
/// recorded with its place, and the case reports it. Any other panic, such
/// as a failed assertion, is printed as usual.
fn quiet_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !IN_CASE.with(Cell::get) {
                return default_hook(info);
            }
            let at = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_default();
            PANICS.with(|p| p.borrow_mut().push(format!("{at}: {message}")));
        }));
    });
}

/// What an input is, for [`run_case`].
pub enum Input<'a> {
    /// A file on disk, opened the way the program opens a path.
    File(&'a Path),
    /// Bytes with a file name, opened the way the program opens bytes.
    Bytes(&'a str, Vec<u8>),
}

/// How much of the program to run after the open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Open, overview, and a page of each sheet.
    Read,
    /// [`Depth::Read`], then a formula and a save to xlsx and to csv.
    Save,
}

/// A case that kept the contract.
#[derive(Debug)]
pub enum Outcome {
    Opened,
    Refused(CommandError),
}

impl Outcome {
    pub fn refused(&self) -> Option<&CommandError> {
        match self {
            Outcome::Refused(e) => Some(e),
            Outcome::Opened => None,
        }
    }
}

fn exercise(kernel: &Kernel, input: Input<'_>, depth: Depth) -> Result<(), CommandError> {
    let book = match input {
        Input::File(path) => kernel.open(path)?,
        Input::Bytes(name, bytes) => kernel.open_bytes(Path::new(name), bytes)?,
    };
    let overview = kernel.overview(&book)?;
    for sheet in overview.sheets.iter().take(8) {
        let range = format!("{}!A1:Z50", format_sheet_name(&sheet.name));
        let request = ReadRequest {
            range: SheetRange::parse(&range).ok(),
            ..ReadRequest::default()
        };
        kernel.read_range(&book, &request)?;
    }
    if depth == Depth::Save {
        kernel.evaluate(&book, None, "SUM(A1:Z50)")?;
        kernel.encode(&book, Path::new("out.xlsx"), FileFormat::Xlsx, None)?;
        kernel.encode(&book, Path::new("out.csv"), FileFormat::Csv, None)?;
    }
    Ok(())
}

/// Runs one input as one command, within the command's budget: the result
/// when the contract held, or what broke it.
pub fn run_case(input: Input<'_>, depth: Depth) -> Result<Outcome, String> {
    quiet_panics();
    PANICS.with(|p| p.borrow_mut().clear());
    let kernel = Kernel::new();
    let start = Instant::now();
    IN_CASE.with(|c| c.set(true));
    kernel.begin_call(None);
    let result = catch_unwind(AssertUnwindSafe(|| exercise(&kernel, input, depth)));
    let ended = kernel.end_call();
    IN_CASE.with(|c| c.set(false));
    let took = start.elapsed();
    let panics = PANICS.with(|p| std::mem::take(&mut *p.borrow_mut()));
    let outcome = match result {
        Err(_) => return Err(format!("panicked outside the kernel's guard at {panics:?}")),
        Ok(Ok(())) => match ended {
            Ok(()) => Outcome::Opened,
            Err(e) => Outcome::Refused(e),
        },
        Ok(Err(e)) => Outcome::Refused(e),
    };
    // A panic caught and swallowed below the kernel counts too, even when
    // the open then went on and worked.
    if !panics.is_empty() {
        return Err(format!("panicked at {panics:?}: {outcome:?}"));
    }
    if let Outcome::Refused(e) = &outcome {
        if e.code == ErrorCode::Internal {
            return Err(format!("internal error: {}", e.why));
        }
        if e.what.trim().is_empty() || e.why.trim().is_empty() || e.fix.trim().is_empty() {
            return Err(format!("an error without all three parts: {e:?}"));
        }
    }
    let limit = time_limit();
    if took > limit {
        return Err(format!("took {took:?}, over the {limit:?} limit"));
    }
    Ok(outcome)
}

/// A known failure, allowed until it is fixed upstream or here.
pub struct Waiver {
    /// The case's name, exactly as the test reports it.
    pub case: &'static str,
    /// A part of the failure's text: the waiver covers only this failure,
    /// so a panic or a slow run of the same case still fails.
    pub problem: &'static str,
    /// What goes wrong, in a few words.
    pub reason: &'static str,
    /// The issue that tracks the fix.
    pub issue: Option<u32>,
    /// Must be [`EXPIRY`].
    pub expires: &'static str,
}

impl Waiver {
    fn covers(&self, case: &str, problem: &str) -> bool {
        self.case == case && problem.contains(self.problem)
    }
}

/// Collects the cases of one test and judges them against its waivers.
#[derive(Default)]
pub struct Report {
    pub cases: Vec<String>,
    pub failures: Vec<(String, String)>,
    pub opened: usize,
    pub refused: usize,
    pub slowest: (Duration, String),
    /// How long the last case took.
    pub last: Duration,
}

impl Report {
    /// Runs one case and records what happened.
    pub fn run(&mut self, case: String, input: Input<'_>, depth: Depth) -> Option<Outcome> {
        let start = Instant::now();
        let result = run_case(input, depth);
        let took = start.elapsed();
        self.last = took;
        if took > self.slowest.0 {
            self.slowest = (took, case.clone());
        }
        self.cases.push(case.clone());
        match result {
            Ok(outcome) => {
                match outcome {
                    Outcome::Opened => self.opened += 1,
                    Outcome::Refused(_) => self.refused += 1,
                }
                Some(outcome)
            }
            Err(problem) => {
                self.failures.push((case, problem));
                None
            }
        }
    }

    /// Records a broken expectation that is not part of the contract
    /// itself, such as a format that should have been refused.
    pub fn fail(&mut self, case: &str, problem: String) {
        self.failures.push((case.to_owned(), problem));
    }

    /// A line for `--nocapture`.
    pub fn summary(&self, what: &str) -> String {
        format!(
            "{what}: {} cases, {} opened, {} refused, {} failed; slowest {:?} ({})",
            self.cases.len(),
            self.opened,
            self.refused,
            self.failures.len(),
            self.slowest.0,
            self.slowest.1
        )
    }

    /// Fails on an unwaived failure, on a waiver no case needs, and on a
    /// waiver without a reason, an issue or the expiry.
    pub fn settle(self, waivers: &[Waiver]) {
        let mut problems = Vec::new();
        for w in waivers {
            if w.reason.trim().is_empty()
                || w.problem.trim().is_empty()
                || w.issue.is_none()
                || w.expires != EXPIRY
            {
                problems.push(format!(
                    "waiver {:?} needs a problem, a reason, an issue and the expiry {EXPIRY}",
                    w.case
                ));
            }
        }
        for (case, problem) in &self.failures {
            if !waivers.iter().any(|w| w.covers(case, problem)) {
                problems.push(format!("{case}: {problem}"));
            }
        }
        for w in waivers {
            let ran = self.cases.iter().any(|c| c == w.case);
            let failed = self.failures.iter().any(|(c, p)| w.covers(c, p));
            if ran && !failed {
                problems.push(format!(
                    "waiver {:?} is not needed any more: remove it",
                    w.case
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "{} problems:\n{}",
            problems.len(),
            problems.join("\n")
        );
    }
}

/// A waiver is checked even when its test is not run (for example when a
/// filter picks other tests): run this from one test of each file.
pub fn check_waivers(waivers: &[Waiver]) {
    for w in waivers {
        assert!(
            !w.reason.trim().is_empty(),
            "waiver {:?}: no reason",
            w.case
        );
        assert!(
            !w.problem.trim().is_empty(),
            "waiver {:?}: no problem",
            w.case
        );
        assert!(w.issue.is_some(), "waiver {:?}: no issue", w.case);
        assert_eq!(w.expires, EXPIRY, "waiver {:?}: wrong expiry", w.case);
    }
}

/// Whether text mentions any of `words`, ignoring case.
pub fn mentions(text: &str, words: &[&str]) -> bool {
    let text = text.to_ascii_lowercase();
    words.iter().any(|w| text.contains(&w.to_ascii_lowercase()))
}

/// The parts of a zip package, in their order, or `None` when it is not a
/// zip (an encrypted package is a compound file).
pub fn unpack(package: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let mut zip = carrysheet_zip::Archive::open(
        std::io::Cursor::new(package),
        carrysheet_zip::Limits::default(),
    )
    .ok()?;
    let names: Vec<String> = zip.entries().iter().map(|e| e.name.clone()).collect();
    let mut parts = Vec::new();
    for (i, name) in names.into_iter().enumerate() {
        parts.push((name, zip.read(i, u64::MAX).ok()?));
    }
    Some(parts)
}

/// A package of `parts`, each stored with its CRC-32.
pub fn pack(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let entries: Vec<_> = parts
        .iter()
        .map(|(name, body)| crate::common::entry(name, body.clone()))
        .collect();
    crate::common::zip(&entries)
}

/// A `.parts` file as a zip package: each `=== name` line starts a part,
/// and lines before the first are a comment.
pub fn load_parts(path: &Path) -> Vec<u8> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut parts: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("=== ") {
            parts.push((name.trim().to_owned(), String::new()));
        } else if let Some((_, body)) = parts.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    let entries: Vec<_> = parts
        .iter()
        .map(|(name, body)| crate::common::entry(name, body.as_bytes()))
        .collect();
    crate::common::zip(&entries)
}
