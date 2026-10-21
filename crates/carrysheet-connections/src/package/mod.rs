//! Reading parts from a package the caller opens.

pub(crate) mod budget;
pub(crate) mod rels;
pub(crate) mod xml;

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::limits::{Limits, Options};
use crate::mask::clean;
use crate::report::Problem;
use budget::{Budget, OutputLimit};
use rels::Relationship;

/// Where the parts of a workbook package come from. The caller implements it
/// over whatever zip reader it already has; this crate never opens files.
pub trait PartSource {
    /// Every part name in the package, as zip entries name them: no leading
    /// `/`, such as `xl/connections.xml`.
    fn part_names(&self) -> Vec<String>;

    /// The uncompressed size of a part, when known without reading it.
    fn part_size(&self, name: &str) -> Option<u64>;

    /// Reads one part. Returns `Ok(None)` when there is no such part, and
    /// [`PartError::TooLarge`] instead of reading more than `max_bytes`.
    fn read_part(&self, name: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, PartError>;
}

/// Why a part could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartError {
    /// The part is larger than the limit the reader asked for.
    TooLarge {
        /// Its size, when known.
        size: Option<u64>,
    },
    /// The part is damaged or could not be read; `reason` says why.
    Unreadable { reason: String },
}

/// Parts held in memory: for tests, and for callers that already have them.
#[derive(Clone, Default)]
pub struct MemoryParts {
    parts: BTreeMap<String, Vec<u8>>,
}

/// Lists part names and sizes only: the bytes may hold secrets, and a debug
/// print must not show them.
impl std::fmt::Debug for MemoryParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.parts.iter().map(|(name, bytes)| (name, bytes.len())))
            .finish()
    }
}

impl MemoryParts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a part. A leading `/` is removed.
    pub fn insert(&mut self, name: &str, bytes: impl Into<Vec<u8>>) {
        self.parts
            .insert(name.trim_start_matches('/').to_string(), bytes.into());
    }
}

impl PartSource for MemoryParts {
    fn part_names(&self) -> Vec<String> {
        self.parts.keys().cloned().collect()
    }

    fn part_size(&self, name: &str) -> Option<u64> {
        self.parts.get(name).map(|bytes| bytes.len() as u64)
    }

    fn read_part(&self, name: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, PartError> {
        match self.parts.get(name) {
            None => Ok(None),
            Some(bytes) if bytes.len() as u64 > max_bytes => Err(PartError::TooLarge {
                size: Some(bytes.len() as u64),
            }),
            Some(bytes) => Ok(Some(bytes.clone())),
        }
    }
}

/// The package as the parsers see it: lookups by name, bounded reads, and
/// the list of problems met on the way.
pub(crate) struct Package<'a> {
    source: &'a dyn PartSource,
    pub limits: &'a Limits,
    pub options: Options,
    /// Lower-cased part name to the name the source uses (OPC part names
    /// compare without case).
    names: HashMap<String, String>,
    pub problems: Vec<Problem>,
    problems_dropped: usize,
    /// Shared by every part; clone the `Rc` to charge it inside a walk.
    pub budget: Rc<Budget>,
}

impl<'a> Package<'a> {
    pub fn new(source: &'a dyn PartSource, limits: &'a Limits, options: Options) -> Self {
        let mut package = Self {
            source,
            limits,
            options,
            names: HashMap::new(),
            problems: Vec::new(),
            problems_dropped: 0,
            budget: Rc::new(Budget::new(limits)),
        };
        let all = source.part_names();
        let total = all.len();
        for name in all.into_iter().take(limits.max_parts) {
            package.names.insert(name.to_ascii_lowercase(), name);
        }
        if total > limits.max_parts {
            package.problem(
                None,
                format!(
                    "The package has {total} parts; only the first {} were looked at.",
                    limits.max_parts
                ),
                "A workbook with that many parts is unusual and could be built to exhaust memory.",
                "Raise Limits::max_parts if the file is trusted.",
            );
        }
        package
    }

    /// Records a problem. Strings are masked and cut like everything else.
    pub fn problem(
        &mut self,
        part: Option<&str>,
        what: impl AsRef<str>,
        why: impl AsRef<str>,
        fix: impl AsRef<str>,
    ) {
        if self.problems.len() >= self.limits.max_items {
            self.problems_dropped += 1;
            return;
        }
        let max = self.limits.max_name_chars;
        // A sentence may hold a whole name, already cut to the name limit
        // with a note of what was cut, so sentences get room past it and
        // the note is never cut off.
        let sentence = max.saturating_add(SENTENCE_CHARS);
        self.problems.push(Problem {
            part: part.map(|p| clean(p, max)),
            what: clean(what.as_ref(), sentence),
            why: clean(why.as_ref(), sentence),
            fix: clean(fix.as_ref(), sentence),
        });
    }

    /// Adds a last problem when some were dropped, and returns them all.
    pub fn take_problems(&mut self) -> Vec<Problem> {
        let mut problems = std::mem::take(&mut self.problems);
        if self.problems_dropped > 0 {
            problems.push(Problem {
                part: None,
                what: format!("{} more problems were not listed.", self.problems_dropped),
                why: "The list of problems is limited like everything else.".to_string(),
                fix: "Fix the problems listed and run again.".to_string(),
            });
        }
        problems
    }

    /// The source's name for `name`, matched without case and with `%XX`
    /// escapes decoded, if the part exists.
    pub fn find(&self, name: &str) -> Option<String> {
        let lower = name.trim_start_matches('/').to_ascii_lowercase();
        self.names
            .get(&lower)
            .or_else(|| self.names.get(&percent_decode(&lower)))
            .cloned()
    }

    /// Part names, sorted, that start with `prefix` and end with `suffix`
    /// (both lower case), such as every `xl/querytables/*.xml`.
    pub fn names_like(&self, prefix: &str, suffix: &str) -> Vec<String> {
        let mut found: Vec<String> = self
            .names
            .iter()
            .filter(|(lower, _)| lower.starts_with(prefix) && lower.ends_with(suffix))
            .map(|(_, name)| name.clone())
            .collect();
        found.sort_by_key(|name| natural_key(name));
        found
    }

    pub fn size(&self, name: &str) -> Option<u64> {
        self.source.part_size(name)
    }

    /// Reads a part of at most `max_bytes`, recording a problem and
    /// returning `None` when it is missing, too large or unreadable. `limit`
    /// is the [`Limit`] that set `max_bytes`, for the problem to name.
    pub fn read(&mut self, name: &str, max_bytes: u64, limit: Limit) -> Option<Vec<u8>> {
        match self.source.read_part(name, max_bytes) {
            Ok(Some(bytes)) if bytes.len() as u64 <= max_bytes => Some(bytes),
            Ok(Some(bytes)) => {
                self.too_large(name, Some(bytes.len() as u64), max_bytes, limit);
                None
            }
            Ok(None) => None,
            Err(PartError::TooLarge { size }) => {
                self.too_large(name, size, max_bytes, limit);
                None
            }
            Err(PartError::Unreadable { reason }) => {
                self.unreadable(name, reason);
                None
            }
        }
    }

    fn unreadable(&mut self, name: &str, reason: String) {
        self.problem(
            Some(name),
            "The part could not be read.",
            reason,
            "Open and save the workbook in Excel to repair it, then try again.",
        );
    }

    fn too_large(&mut self, name: &str, size: Option<u64>, max_bytes: u64, limit: Limit) {
        let size = size.map_or_else(
            || format!("more than {max_bytes} bytes"),
            |s| format!("{s} bytes"),
        );
        let Limit { name: field, value } = limit;
        let why = if max_bytes >= value {
            format!(
                "{field} is {value} bytes; larger parts are not read, so a hostile file cannot exhaust memory."
            )
        } else {
            format!(
                "{field} ({value} bytes) covers these parts together, and only {max_bytes} bytes of it were left."
            )
        };
        self.problem(
            Some(name),
            format!("The part was skipped: it is {size}."),
            why,
            format!("Raise {field} if the file is trusted."),
        );
    }

    /// Reads and decodes an XML part within the XML size limit and what is
    /// left of the total XML budget.
    pub fn read_xml(&mut self, name: &str) -> Option<String> {
        let left = self.budget.xml_bytes_left();
        if left == 0 {
            self.budget.stop_reading();
            return None;
        }
        let limit = self.limits.max_xml_part_bytes;
        if left < limit {
            // The budget, not the part limit, decides: a part too large for
            // what is left stops reading instead of being reported as large.
            return match self.source.read_part(name, left) {
                Ok(Some(bytes)) if self.budget.read(bytes.len() as u64) => self.decode(name, bytes),
                Ok(Some(_)) | Err(PartError::TooLarge { .. }) => {
                    self.budget.stop_reading();
                    None
                }
                Ok(None) => None,
                Err(PartError::Unreadable { reason }) => {
                    self.unreadable(name, reason);
                    None
                }
            };
        }
        let bytes = self.read(
            name,
            limit,
            Limit {
                name: "Limits::max_xml_part_bytes",
                value: limit,
            },
        )?;
        if !self.budget.read(bytes.len() as u64) {
            return None;
        }
        self.decode(name, bytes)
    }

    /// Adds the problems for budgets that ran out. Call once, at the end.
    pub fn report_budgets(&mut self) {
        if self.budget.reading_spent() {
            self.problem(
                None,
                format!(
                    "Reading stopped after {} bytes of XML; parts after that are left out.",
                    self.limits.max_total_xml_bytes
                ),
                "All XML parts together are read only up to a limit, so a hostile file cannot exhaust memory or time.",
                "Raise Limits::max_total_xml_bytes if the file is trusted.",
            );
        }
        if let Some(hit) = self.budget.output_hit() {
            let (field, value, unit) = match hit {
                OutputLimit::Items => (
                    "Limits::max_total_items",
                    self.limits.max_total_items,
                    "items",
                ),
                OutputLimit::Chars => (
                    "Limits::max_total_chars",
                    self.limits.max_total_chars,
                    "characters of text",
                ),
            };
            self.problem(
                None,
                format!("The report reached {field} ({value} {unit}); later items are left out."),
                "The whole report is limited, so a file with many parts cannot make it unbounded.",
                format!("Raise {field} if the file is trusted."),
            );
        }
    }

    pub fn decode(&mut self, name: &str, bytes: Vec<u8>) -> Option<String> {
        match xml::decode(bytes) {
            Ok(text) => Some(text),
            Err(why) => {
                self.xml_problem(name, &why);
                None
            }
        }
    }

    /// Records that an XML part is malformed.
    pub fn xml_problem(&mut self, name: &str, why: &str) {
        self.problem(
            Some(name),
            "The part's XML could not be read; what it says is left out.",
            why,
            "Open and save the workbook in Excel to repair it, then try again.",
        );
    }

    /// The relationships of `part` (`""` for the package root). Empty when
    /// the part has none.
    pub fn relationships(&mut self, part: &str) -> Vec<Relationship> {
        let Some(name) = self.find(&rels::rels_name(part)) else {
            return Vec::new();
        };
        let Some(text) = self.read_xml(&name) else {
            return Vec::new();
        };
        match rels::parse(&text, self.limits.max_depth, self.limits.max_items) {
            Ok((list, truncated)) => {
                if truncated {
                    self.too_many(&name, "relationships");
                }
                list
            }
            Err(why) => {
                self.xml_problem(&name, &why);
                Vec::new()
            }
        }
    }

    /// Records that a part lists more of something than the limit.
    pub fn too_many(&mut self, part: &str, what: &str) {
        let max = self.limits.max_items;
        self.problem(
            Some(part),
            format!("Only the first {max} {what} were read."),
            format!("The part lists more {what} than Limits::max_items ({max})."),
            "Raise Limits::max_items if the file is trusted.",
        );
    }

    /// The part an internal relationship of `source` points at, if it exists.
    pub fn target(&self, source: &str, relationship: &Relationship) -> Option<String> {
        if relationship.external {
            return None;
        }
        rels::resolve(source, &relationship.target).and_then(|name| self.find(&name))
    }
}

/// Sort key that puts `item2` before `item10`.
fn natural_key(name: &str) -> (String, u64, String) {
    let lower = name.to_ascii_lowercase();
    let digits_end = lower.rfind(|c: char| c.is_ascii_digit()).map(|i| i + 1);
    let Some(end) = digits_end else {
        return (lower, 0, String::new());
    };
    let start = lower[..end]
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let number = lower[start..end].parse().unwrap_or(u64::MAX);
    (lower[..start].to_string(), number, lower[end..].to_string())
}

fn percent_decode(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = bytes
                .get(i + 1..i + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_ascii_lowercase()
}

/// Room in a problem's sentence besides a name it holds.
const SENTENCE_CHARS: usize = 1_000;

/// A setting in [`crate::Limits`], named in problems about it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limit {
    pub name: &'static str,
    pub value: u64,
}
