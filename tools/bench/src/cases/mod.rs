//! The cases: what each one reads and what it times.
//!
//! A case is a name, an input and an operation. `setup` gets the input's
//! bytes, does any work that should not be timed (such as reading a file
//! before timing its save), and returns the operation. The operation
//! returns a number derived from its result (rows read, bytes written), so
//! the work cannot be optimized away and a change in what it computes shows
//! up in the output as a different `check`.
//!
//! To add an area (such as the kernel's open, read a range, edit one cell
//! plus recalculate, save), add one file here with a `cases()` function
//! like the others and list it in [`all`].

use crate::generate::{self as make, Generated};

mod biff;
mod crypto;
mod csv;
mod numbers;
mod vba;

/// The timed operation. Returns a check value.
pub type Op = Box<dyn FnMut() -> Result<u64, String>>;

/// One benchmark case.
pub struct Case {
    /// `file/operation`, such as `csv-100mb/open`. Stable: the baseline
    /// is keyed by it. The part before the slash names the file (or file
    /// set); the part after is [`Case::op`].
    pub name: &'static str,
    /// What the case does to the file.
    pub op: Operation,
    /// One line for `bench list`.
    pub about: &'static str,
    pub input: Input,
    /// Times the operation runs per timed sample, so a fast operation is
    /// timed over long enough to measure. Times are reported per run.
    pub inner: u32,
    /// Prepares the operation from the input files' bytes.
    pub setup: fn(Vec<Vec<u8>>) -> Result<Op, String>,
}

/// What a case does to its file. Open, save and recalculate are the
/// operations people feel; issue 72 (parallel reading, writing and MCP)
/// compares against these single-thread numbers per file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Read the file into memory.
    Open,
    /// Read a range of cells from a file opened untimed.
    ReadRange,
    /// Write a file opened untimed back out.
    Save,
    /// Recalculate after editing one cell. Needs the kernel (issue 45).
    Recalc,
    /// Find out how a file is protected.
    Detect,
    /// Decrypt a protected file.
    Decrypt,
    /// Decompress a stream.
    Decompress,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Open => "open",
            Operation::ReadRange => "read-range",
            Operation::Save => "save",
            Operation::Recalc => "recalc",
            Operation::Detect => "detect",
            Operation::Decrypt => "decrypt",
            Operation::Decompress => "decompress",
        }
    }
}

impl Case {
    /// The file (or file set) part of the name.
    pub fn file(&self) -> &'static str {
        self.name.split_once('/').map_or(self.name, |(f, _)| f)
    }
}

/// What a case reads.
pub enum Input {
    /// Committed files: every file under these folders (relative to the
    /// repository root) with one of these extensions, in sorted order.
    Files {
        dirs: &'static [&'static str],
        exts: &'static [&'static str],
    },
    /// A generated file.
    Generated(&'static Generated),
    /// Nothing; the case builds its input in `setup`.
    None,
}

/// Every case, in the order they run.
pub fn all() -> Vec<Case> {
    let mut v = Vec::new();
    v.extend(csv::cases());
    v.extend(biff::cases());
    v.extend(numbers::cases());
    v.extend(vba::cases());
    v.extend(crypto::cases());
    v
}

/// The generated inputs the cases use, each once.
pub fn generated() -> Vec<&'static Generated> {
    let mut v: Vec<&'static Generated> = Vec::new();
    for case in all() {
        if let Input::Generated(g) = case.input
            && !v.iter().any(|h| std::ptr::eq(*h, g))
        {
            v.push(g);
        }
    }
    v
}

pub static CSV_100MB: Generated = Generated {
    stem: "csv-100mb",
    ext: "csv",
    make: make::csv_100mb,
    salted: false,
};
pub static CSV_WIDE: Generated = Generated {
    stem: "csv-wide",
    ext: "csv",
    make: make::csv_wide,
    salted: false,
};
pub static CSV_QUOTES: Generated = Generated {
    stem: "csv-hostile-quotes",
    ext: "csv",
    make: make::csv_quotes,
    salted: false,
};
pub static CSV_EMPTY: Generated = Generated {
    stem: "csv-hostile-empty-cells",
    ext: "csv",
    make: make::csv_empty_cells,
    salted: false,
};
pub static CSV_GBK: Generated = Generated {
    stem: "csv-gbk-20mb",
    ext: "csv",
    make: make::csv_gbk,
    salted: false,
};
pub static XLS_1M: Generated = Generated {
    stem: "xls-1m-cells",
    ext: "xls",
    make: make::xls_1m_cells,
    salted: false,
};
pub static XLS_RECORDS: Generated = Generated {
    stem: "xls-hostile-many-records",
    ext: "xls",
    make: make::xls_many_records,
    salted: false,
};
pub static XLS_RC4: Generated = Generated {
    stem: "xls-1m-cells-rc4",
    ext: "bin",
    make: make::xls_rc4_stream,
    salted: true,
};
pub static VBA_BOMB: Generated = Generated {
    stem: "vba-hostile-bomb-module",
    ext: "bin",
    make: make::vba_bomb_module,
    salted: false,
};

/// Adds up a list of results: the sum of the `Ok` values, plus one for
/// every error, so a file that starts or stops failing changes the check.
fn tally<T>(
    results: impl Iterator<Item = Result<T, impl std::fmt::Display>>,
    f: impl Fn(&T) -> u64,
) -> u64 {
    results
        .map(|r| match r {
            Ok(v) => f(&v) << 1,
            Err(_) => 1,
        })
        .sum()
}

/// Takes the only input file.
fn one(mut inputs: Vec<Vec<u8>>) -> Result<Vec<u8>, String> {
    match inputs.len() {
        1 => Ok(inputs.pop().expect("one input")),
        n => Err(format!("expected one input file, got {n}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_shaped() {
        let cases = all();
        for (i, c) in cases.iter().enumerate() {
            assert_eq!(
                c.name.split_once('/').map(|(_, op)| op),
                Some(c.op.as_str()),
                "{}",
                c.name
            );
            assert!(c.inner >= 1, "{}", c.name);
            assert!(
                cases[..i].iter().all(|d| d.name != c.name),
                "duplicate {}",
                c.name
            );
        }
    }

    #[test]
    fn generated_names_are_unique() {
        let g = generated();
        for (i, a) in g.iter().enumerate() {
            assert!(g[..i].iter().all(|b| b.file_name() != a.file_name()));
        }
    }
}
