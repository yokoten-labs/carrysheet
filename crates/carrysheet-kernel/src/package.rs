//! Comparing two OOXML packages part by part, and counting the features a
//! save must keep (record 0003, point 3). The approach follows the kernel
//! bench's A5 check (research/kernel-bench on branch kernel-bench).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use carrysheet_commands::engine::save::MAX_LISTED_PARTS;
use carrysheet_commands::engine::{Loss, PartCounts, SaveReport};
use carrysheet_zip::{Archive, DuplicateNames, Limits as ZipLimits};
use gridcraft_engine::core::limits as gc;

/// Most entries listed from one package (as GridCraft reads).
const MAX_ENTRIES: usize = 200_000;
/// Largest central directory read: room for [`MAX_ENTRIES`] entries with
/// names of a few hundred bytes.
const MAX_DIRECTORY: u64 = 64 << 20;
/// Largest part searched for features inside sheets and the workbook.
const MAX_SCANNED_PART: u64 = 64 << 20;
/// Most bytes searched in one package.
const MAX_SCANNED_TOTAL: u64 = 512 << 20;

/// Parts whose loss does not matter: caches Excel rebuilds.
const HARMLESS: &[&str] = &["xl/calcchain.xml"];

/// One part as the zip directory describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    index: usize,
    crc: u32,
    size: u64,
}

/// Why some of a package was not searched, as the loss text says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unsearched {
    /// Over the part or package cap.
    TooLarge,
    /// A part's compressed data is shaped to cost time (such as runs of
    /// empty deflate blocks), and the zip reader refused it (issue 80).
    Slow,
    /// A part could not be unpacked: damaged, or a zip feature Excel does
    /// not write.
    Unreadable,
    /// The command's time limit ran out while unpacking.
    OutOfTime,
}

impl Unsearched {
    fn text(self) -> &'static str {
        match self {
            Unsearched::TooLarge => "the file is too large to search",
            Unsearched::Slow => {
                "a part was refused because it unpacks far more slowly than real files do, as files built to waste time are"
            }
            Unsearched::Unreadable => "a part could not be unpacked; it may be damaged",
            Unsearched::OutOfTime => "the command's time limit ran out while unpacking parts",
        }
    }
}

/// A package's parts by normalized name (lower case, no leading `/`).
pub(crate) struct Package<'a> {
    zip: Archive<Cursor<&'a [u8]>>,
    parts: BTreeMap<String, (String, Entry)>,
    /// Parts already refused by [`Package::unpack`], with why: each search
    /// of the package asks again, and must not pay for the same refusal
    /// twice.
    refused: BTreeMap<usize, Unsearched>,
    /// Why searches of this package stopped short, if any did.
    unsearched: BTreeSet<Unsearched>,
    caps: Caps,
}

/// How much of a package is searched: [`Caps::DEFAULT`] outside tests.
#[derive(Debug, Clone, Copy)]
struct Caps {
    part: u64,
    total: u64,
}

impl Caps {
    const DEFAULT: Caps = Caps {
        part: MAX_SCANNED_PART,
        total: MAX_SCANNED_TOTAL,
    };
}

fn normalize(name: &str) -> String {
    name.replace('\\', "/")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

/// The zip reader's limits for the save check. Each search may unpack the
/// same parts again, so the reader's totals are not the bound: [`Caps`]
/// bound each search, the reader's guards bound the work per byte, and the
/// command's clock (the interrupt) bounds the time.
fn zip_limits() -> ZipLimits {
    ZipLimits {
        max_entries: MAX_ENTRIES,
        max_directory_bytes: MAX_DIRECTORY,
        max_entry_bytes: MAX_SCANNED_PART,
        max_total_bytes: u64::MAX,
        max_total_read: u64::MAX,
        // Listed as GridCraft lists them: the first of two same names.
        duplicate_names: DuplicateNames::Report,
        ..ZipLimits::default()
    }
}

impl<'a> Package<'a> {
    /// Lists a package's parts without unpacking them; `None` if the bytes
    /// are not a zip the hardened reader opens.
    pub(crate) fn open(bytes: &'a [u8]) -> Option<Package<'a>> {
        let mut zip = Archive::open(Cursor::new(bytes), zip_limits()).ok()?;
        // Unpacking counts against the command's time limit (round 2, N6).
        zip.set_interrupt(Box::new(|| !gc::check_time()));
        let mut parts = BTreeMap::new();
        for (index, f) in zip.entries().iter().enumerate() {
            if f.is_dir() {
                continue;
            }
            let entry = Entry {
                index,
                crc: f.crc32,
                size: f.size,
            };
            parts
                .entry(normalize(&f.name))
                .or_insert((f.name.clone(), entry));
        }
        Some(Package {
            zip,
            parts,
            refused: BTreeMap::new(),
            unsearched: BTreeSet::new(),
            caps: Caps::DEFAULT,
        })
    }

    /// The unpacked bytes of one part (at most the part cap); `None` when
    /// it cannot be read, is shaped to unpack slowly, or the command's time
    /// ran out (round 2, N6: slow unpacking). Why is kept in `unsearched`.
    fn unpack(&mut self, e: Entry) -> Option<Vec<u8>> {
        if let Some(why) = self.refused.get(&e.index) {
            self.unsearched.insert(*why);
            return None;
        }
        if !gc::check_time() {
            self.unsearched.insert(Unsearched::OutOfTime);
            return None;
        }
        match self.zip.read(e.index, self.caps.part) {
            Ok(data) => Some(data),
            Err(err) => {
                let why = match err {
                    carrysheet_zip::Error::Interrupted => Unsearched::OutOfTime,
                    carrysheet_zip::Error::LimitExceeded { .. } => Unsearched::TooLarge,
                    ref z if z.is_slow_to_unpack() => Unsearched::Slow,
                    _ => Unsearched::Unreadable,
                };
                self.refused.insert(e.index, why);
                self.unsearched.insert(why);
                None
            }
        }
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.parts.keys().map(String::as_str)
    }

    /// Times each needle appears in the parts `pick` selects, with a cap
    /// on what is unpacked. `None` when the cap was reached.
    fn count_in(&mut self, pick: impl Fn(&str) -> bool, needles: &[&[u8]]) -> Option<Vec<u64>> {
        let mut counts = vec![0u64; needles.len()];
        let mut total = 0u64;
        let picked: Vec<Entry> = self
            .parts
            .iter()
            .filter(|(n, _)| pick(n))
            .map(|(_, (_, e))| *e)
            .collect();
        for e in picked {
            if e.size > self.caps.part || total + e.size > self.caps.total {
                self.unsearched.insert(Unsearched::TooLarge);
                return None;
            }
            let data = self.unpack(e)?;
            total += data.len() as u64;
            for (n, needle) in needles.iter().enumerate() {
                counts[n] += count(&data, needle);
            }
        }
        Some(counts)
    }

    /// Calls `f` with each part `pick` selects, with the same caps as
    /// [`Package::count_in`]. `false` when the cap was reached.
    fn each(&mut self, pick: impl Fn(&str) -> bool, f: &mut dyn FnMut(&[u8])) -> bool {
        let mut total = 0u64;
        let picked: Vec<Entry> = self
            .parts
            .iter()
            .filter(|(n, _)| pick(n))
            .map(|(_, (_, e))| *e)
            .collect();
        for e in picked {
            if e.size > self.caps.part || total + e.size > self.caps.total {
                self.unsearched.insert(Unsearched::TooLarge);
                return false;
            }
            let Some(data) = self.unpack(e) else {
                return false;
            };
            total += data.len() as u64;
            f(&data);
        }
        true
    }
}

// ------------------------------------------------------------ content gate
//
// Part names and counts miss content lost inside a part GridCraft rewrites
// (review H4: a code name, a data table formula, a person's id). The gate
// below takes, from both packages, the watched elements with the
// attributes that carry their meaning, and reports what the output lacks.

/// A watched element: the feature it belongs to, the part it is in, its
/// local name, and the attributes compared (`None`: all of them).
struct Watch {
    feature: &'static str,
    part: fn(&str) -> bool,
    element: &'static str,
    attrs: Option<&'static [&'static str]>,
}

fn sheet_part(n: &str) -> bool {
    is(n, "xl/worksheets/", "sheet")
}
fn workbook_part(n: &str) -> bool {
    n == "xl/workbook.xml"
}
fn strings_part(n: &str) -> bool {
    n == "xl/sharedstrings.xml"
}
fn comments_part(n: &str) -> bool {
    n.starts_with("xl/comments") && n.ends_with(".xml") && !n.contains("/_rels/")
}
fn persons_part(n: &str) -> bool {
    is(n, "xl/persons/", "")
}

const WATCHED: &[Watch] = &[
    Watch {
        feature: "file sharing settings",
        part: workbook_part,
        element: "fileSharing",
        attrs: None,
    },
    Watch {
        feature: "workbook code name (VBA)",
        part: workbook_part,
        element: "workbookPr",
        attrs: Some(&["codeName"]),
    },
    Watch {
        feature: "sheet code names (VBA)",
        part: sheet_part,
        element: "sheetPr",
        attrs: Some(&["codeName"]),
    },
    Watch {
        feature: "what-if data tables",
        part: sheet_part,
        element: "f",
        attrs: Some(&["t=dataTable", "ref", "dt2D", "dtr", "r1", "r2"]),
    },
    Watch {
        feature: "scenarios",
        part: sheet_part,
        element: "scenario",
        attrs: Some(&["name", "count", "user", "comment"]),
    },
    Watch {
        feature: "scenarios",
        part: sheet_part,
        element: "inputCells",
        attrs: Some(&["r", "val"]),
    },
    Watch {
        feature: "AutoFilter settings",
        part: sheet_part,
        element: "autoFilter",
        attrs: Some(&["ref"]),
    },
    Watch {
        feature: "AutoFilter settings",
        part: sheet_part,
        element: "filter",
        attrs: Some(&["val"]),
    },
    Watch {
        feature: "AutoFilter settings",
        part: sheet_part,
        element: "customFilter",
        attrs: Some(&["val"]),
    },
    Watch {
        feature: "AutoFilter settings",
        part: sheet_part,
        element: "dynamicFilter",
        attrs: Some(&["type"]),
    },
    Watch {
        feature: "AutoFilter settings",
        part: sheet_part,
        element: "dateGroupItem",
        attrs: None,
    },
    Watch {
        feature: "AutoFilter sort",
        part: sheet_part,
        element: "sortCondition",
        attrs: Some(&["ref", "descending"]),
    },
    Watch {
        feature: "phonetic settings",
        part: sheet_part,
        element: "phoneticPr",
        attrs: None,
    },
    Watch {
        feature: "ignored error flags",
        part: sheet_part,
        element: "ignoredError",
        attrs: None,
    },
    Watch {
        feature: "hyperlink text and tips",
        part: sheet_part,
        element: "hyperlink",
        attrs: Some(&["ref", "display", "tooltip"]),
    },
    Watch {
        feature: "rich text in cells",
        part: strings_part,
        element: "rPr",
        attrs: Some(&[]),
    },
    Watch {
        feature: "phonetic guides",
        part: strings_part,
        element: "rPh",
        attrs: None,
    },
    Watch {
        feature: "formatted note text",
        part: comments_part,
        element: "rPr",
        attrs: Some(&[]),
    },
    Watch {
        feature: "people of threaded comments (user ids)",
        part: persons_part,
        element: "person",
        attrs: Some(&["displayName", "userId", "providerId"]),
    },
];

/// The watched elements of a package, as (feature, signature) counts. `None`
/// when the package was too large to search.
type Signatures = BTreeMap<(&'static str, String), u64>;

fn signatures(p: &mut Package<'_>) -> Option<Signatures> {
    let mut out = Signatures::new();
    let parts: [fn(&str) -> bool; 5] = [
        workbook_part,
        sheet_part,
        strings_part,
        comments_part,
        persons_part,
    ];
    for part in parts {
        let watches: Vec<&Watch> = WATCHED
            .iter()
            .filter(|w| std::ptr::fn_addr_eq(w.part, part))
            .collect();
        let complete = p.each(part, &mut |data| {
            for_each_tag(data, &mut |name, attrs| {
                for w in watches.iter().filter(|w| w.element == name) {
                    if let Some(sig) = signature(w, attrs) {
                        *out.entry((w.feature, sig)).or_default() += 1;
                    }
                }
            });
        });
        if !complete {
            return None;
        }
    }
    Some(out)
}

/// The signature of one element for `w`, or `None` when it is not one
/// `w` watches (an `attr=value` entry must match).
fn signature(w: &Watch, attrs: &[(&str, String)]) -> Option<String> {
    let mut sig = String::new();
    match w.attrs {
        None => {
            let mut all: Vec<String> = attrs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            all.sort_unstable();
            sig = all.join(" ");
        }
        Some(keys) => {
            for key in keys {
                if let Some((k, want)) = key.split_once('=') {
                    let v = attrs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
                    if v != Some(want) {
                        return None;
                    }
                    continue;
                }
                if let Some((_, v)) = attrs.iter().find(|(n, _)| n == key) {
                    sig.push_str(key);
                    sig.push('=');
                    sig.push_str(v);
                    sig.push(' ');
                }
            }
        }
    }
    Some(sig)
}

/// What [`for_each_tag`] calls: a tag's local name and its attributes.
type TagFn<'f> = dyn FnMut(&str, &[(&str, String)]) + 'f;

/// Calls `f` with the local name and attributes (local names, values with
/// entities decoded) of each start tag in `data`. A light scanner, not a
/// parser: GridCraft has read both packages already, so they are well
/// formed; comments, CDATA and processing instructions are skipped.
fn for_each_tag(data: &[u8], f: &mut TagFn<'_>) {
    let text = String::from_utf8_lossy(data);
    let s = text.as_ref();
    let mut i = 0;
    let mut attrs: Vec<(&str, String)> = Vec::new();
    while let Some(at) = s.get(i..).and_then(|r| r.find('<')) {
        let start = i + at + 1;
        let rest = s.get(start..).unwrap_or("");
        if rest.starts_with("!--") {
            i = start + rest.find("-->").map_or(rest.len(), |e| e + 3);
            continue;
        }
        if rest.starts_with("![CDATA[") {
            i = start + rest.find("]]>").map_or(rest.len(), |e| e + 3);
            continue;
        }
        if rest.starts_with('/') || rest.starts_with('?') || rest.starts_with('!') {
            i = start + 1;
            continue;
        }
        // The tag runs to the first `>` outside quotes.
        let mut end = rest.len();
        let mut quote: Option<char> = None;
        for (j, c) in rest.char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '>') => {
                    end = j;
                    break;
                }
                _ => {}
            }
        }
        let tag = rest.get(..end).unwrap_or("").trim_end_matches('/');
        let name_end = tag
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(tag.len());
        let name = local(tag.get(..name_end).unwrap_or(""));
        attrs.clear();
        let mut a = tag.get(name_end..).unwrap_or("");
        while let Some(eq) = a.find('=') {
            let key = a.get(..eq).unwrap_or("").trim();
            let after = a.get(eq + 1..).unwrap_or("").trim_start();
            let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                break;
            };
            let body = after.get(1..).unwrap_or("");
            let close = body.find(q).unwrap_or(body.len());
            if !key.starts_with("xmlns") {
                attrs.push((local(key), unescape(body.get(..close).unwrap_or(""))));
            }
            a = body.get(close + 1..).unwrap_or("");
        }
        f(name, &attrs);
        i = start + end;
    }
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn unescape(v: &str) -> String {
    if !v.contains('&') {
        return v.to_owned();
    }
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or(""));
        let tail = rest.get(at + 1..).unwrap_or("");
        let Some(semi) = tail.find(';').filter(|n| *n <= 10) else {
            out.push('&');
            rest = tail;
            continue;
        };
        let ent = tail.get(..semi).unwrap_or("");
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => out.push(c),
            None => {
                out.push('&');
                out.push_str(ent);
                out.push(';');
            }
        }
        rest = tail.get(semi + 1..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// What the output lacks of the watched content, by feature.
fn content_losses(before: &mut Package<'_>, after: &mut Package<'_>, report: &mut SaveReport) {
    let (Some(had), Some(has)) = (signatures(before), signatures(after)) else {
        // Not searched (too large, or unreadable): a loss, so the save
        // needs confirm_loss (round 2, N4).
        report.lost.push(unchecked_content(
            "content inside rewritten parts",
            &why_unsearched(before, after),
        ));
        return;
    };
    let mut lost: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new();
    for ((feature, sig), n) in &had {
        let kept = has.get(&(*feature, sig.clone())).copied().unwrap_or(0);
        let e = lost.entry(feature).or_default();
        e.0 += n;
        e.1 += kept.min(*n);
    }
    for (feature, (before, after)) in lost {
        if after < before {
            report.lost.push(Loss {
                feature: feature.to_owned(),
                before,
                after,
            });
        }
    }
    // Formulas GridCraft cannot compute must keep their saved values.
    let names = |p: &mut Package<'_>| p.count_in(sheet_part, &[b"<v>#NAME?</v>", b"_xlfn."]);
    let (Some(b), Some(a)) = (names(before), names(after)) else {
        report.lost.push(unchecked_content(
            "saved results of formulas",
            &why_unsearched(before, after),
        ));
        return;
    };
    {
        if a[0] > b[0] {
            report.lost.push(Loss {
                feature: "formula results (they would become #NAME?)".into(),
                before: a[0] - b[0],
                after: 0,
            });
        }
        if a[1] < b[1] {
            report.lost.push(Loss {
                feature: "newer Excel function names (_xlfn.)".into(),
                before: b[1],
                after: a[1],
            });
        }
    }
}

fn count(hay: &[u8], needle: &[u8]) -> u64 {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    let first = needle[0];
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i] == first && hay[i..].starts_with(needle) {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

/// `xl/charts/chart3.xml` is in `dir` with a name starting `stem`, and not a
/// relationship part.
fn is(name: &str, dir: &str, stem: &str) -> bool {
    name.strip_prefix(dir)
        .is_some_and(|rest| rest.starts_with(stem) && !rest.contains('/'))
}

/// A feature found by part name: its label and the test on a part name.
type PartFeature = (&'static str, fn(&str) -> bool);

/// Features found by part name.
const PART_FEATURES: &[PartFeature] = &[
    ("VBA project", |n| n == "xl/vbaproject.bin"),
    ("charts", |n| is(n, "xl/charts/", "chart")),
    ("chart sheets", |n| is(n, "xl/chartsheets/", "sheet")),
    ("pivot tables", |n| is(n, "xl/pivottables/", "pivottable")),
    ("pivot caches", |n| {
        is(n, "xl/pivotcache/", "pivotcachedefinition")
    }),
    ("slicers", |n| {
        is(n, "xl/slicers/", "") || is(n, "xl/slicercaches/", "")
    }),
    ("timelines", |n| {
        is(n, "xl/timelines/", "") || is(n, "xl/timelinecaches/", "")
    }),
    ("comments", |n| {
        n.starts_with("xl/comments") && n.ends_with(".xml") && !n.contains("/_rels/")
    }),
    ("threaded comments", |n| is(n, "xl/threadedcomments/", "")),
    ("notes and form controls (VML)", |n| {
        is(n, "xl/drawings/", "vmldrawing")
    }),
    ("drawings", |n| is(n, "xl/drawings/", "drawing")),
    ("images", |n| is(n, "xl/media/", "")),
    ("tables", |n| is(n, "xl/tables/", "table")),
    ("external links", |n| {
        is(n, "xl/externallinks/", "externallink")
    }),
    ("data connections", |n| n == "xl/connections.xml"),
    ("query tables", |n| is(n, "xl/querytables/", "")),
    ("embedded objects", |n| is(n, "xl/embeddings/", "")),
    ("ActiveX controls", |n| is(n, "xl/activex/", "")),
    ("custom XML", |n| {
        is(n, "customxml/", "item") && !n.contains("props")
    }),
    ("custom document properties", |n| n == "docprops/custom.xml"),
    ("macro sheets (Excel 4)", |n| is(n, "xl/macrosheets/", "")),
    ("dialog sheets", |n| is(n, "xl/dialogsheets/", "")),
    ("data model", |n| n.starts_with("xl/model/")),
    ("cell images and rich data", |n| is(n, "xl/richdata/", "")),
    ("web extensions", |n| is(n, "xl/webextensions/", "")),
    ("digital signatures", |n| n.starts_with("_xmlsignatures/")),
];

/// Features found inside sheets, as (label, needles).
const SHEET_FEATURES: &[(&str, &[&[u8]])] = &[
    (
        "conditional formatting",
        &[b"<conditionalFormatting ", b"<x14:conditionalFormatting "],
    ),
    (
        "data validation",
        &[b"<dataValidation ", b"<x14:dataValidation "],
    ),
    ("sparklines", &[b"<x14:sparklineGroup "]),
    ("hyperlinks", &[b"<hyperlink "]),
    ("merged cells", &[b"<mergeCell "]),
];

/// Counts every feature in a package. The second list names features it
/// could not count (too large to search).
fn features(p: &mut Package<'_>) -> (BTreeMap<&'static str, u64>, Vec<&'static str>) {
    let mut m = BTreeMap::new();
    let mut unchecked = Vec::new();
    for (label, test) in PART_FEATURES {
        let n = p.names().filter(|n| test(n)).count() as u64;
        if n > 0 {
            *m.entry(*label).or_default() += n;
        }
    }
    let sheet = |n: &str| is(n, "xl/worksheets/", "sheet");
    let needles: Vec<&[u8]> = SHEET_FEATURES
        .iter()
        .flat_map(|(_, ns)| ns.iter().copied())
        .collect();
    match p.count_in(sheet, &needles) {
        Some(counts) => {
            let mut at = 0;
            for (label, ns) in SHEET_FEATURES {
                let n: u64 = counts[at..at + ns.len()].iter().sum();
                at += ns.len();
                if n > 0 {
                    m.insert(*label, n);
                }
            }
        }
        None => unchecked.extend(SHEET_FEATURES.iter().map(|(l, _)| *l)),
    }
    match p.count_in(|n| n == "xl/workbook.xml", &[b"<definedName "]) {
        Some(c) if c[0] > 0 => {
            m.insert("defined names", c[0]);
        }
        Some(_) => {}
        None => unchecked.push("defined names"),
    }
    (m, unchecked)
}

/// Feature counts of a package, for reports when the output is not one.
/// `None` when its parts cannot be listed.
pub(crate) fn source_features(bytes: &[u8]) -> Option<BTreeMap<&'static str, u64>> {
    Package::open(bytes).map(|mut p| features(&mut p).0)
}

/// The loss reported when a package cannot be compared: the save must not
/// pass as losing nothing (record 0003).
pub(crate) fn unchecked_loss() -> Loss {
    Loss {
        feature:
            "anything the file holds besides cell values (its parts could not be listed to check)"
                .into(),
        before: 1,
        after: 0,
    }
}

/// The loss reported for features the gate could not search (the file is
/// too large, or a part could not be unpacked): like [`unchecked_loss`],
/// the save must not pass as losing nothing (round 2, N4). The text names
/// the real reasons (issue 80): a part refused for unpacking slowly is not
/// a file too large to search.
fn unchecked_content(what: &str, why: &BTreeSet<Unsearched>) -> Loss {
    let why = if why.is_empty() {
        Unsearched::TooLarge.text().to_owned()
    } else {
        why.iter().map(|w| w.text()).collect::<Vec<_>>().join("; ")
    };
    Loss {
        feature: format!("{what} (not checked: {why})"),
        before: 1,
        after: 0,
    }
}

/// Why searches of either package stopped short.
fn why_unsearched(before: &Package<'_>, after: &Package<'_>) -> BTreeSet<Unsearched> {
    before
        .unsearched
        .union(&after.unsearched)
        .copied()
        .collect()
}

/// Compares the opened package with the one about to be written.
pub(crate) fn compare(source: &[u8], output: &[u8], report: &mut SaveReport) {
    compare_within(source, output, report, Caps::DEFAULT);
}

fn compare_within(source: &[u8], output: &[u8], report: &mut SaveReport, caps: Caps) {
    let (Some(mut before), Some(mut after)) = (Package::open(source), Package::open(output)) else {
        report.lost.push(unchecked_loss());
        return;
    };
    before.caps = caps;
    after.caps = caps;
    let mut counts = PartCounts::default();
    for (name, (shown, e)) in &before.parts {
        match after.parts.get(name) {
            Some((_, a)) if a.crc == e.crc && a.size == e.size => counts.identical += 1,
            Some(_) => counts.rewritten += 1,
            None if HARMLESS.contains(&name.as_str()) => {
                counts.dropped += 1;
                report.notes.push(format!(
                    "{} is not written; Excel rebuilds it",
                    carrysheet_commands::engine::run::part_name(shown)
                ));
            }
            None => {
                counts.dropped += 1;
                if report.dropped_parts.len() < MAX_LISTED_PARTS {
                    report
                        .dropped_parts
                        .push(carrysheet_commands::engine::run::part_name(shown));
                }
            }
        }
    }
    counts.added = after
        .parts
        .keys()
        .filter(|k| !before.parts.contains_key(*k))
        .count() as u64;
    report.parts = Some(counts);

    let (had, unchecked_before) = features(&mut before);
    let (has, unchecked_after) = features(&mut after);
    for (feature, n) in &had {
        let kept = has.get(feature).copied().unwrap_or(0);
        if kept < *n {
            report.lost.push(Loss {
                feature: (*feature).to_owned(),
                before: *n,
                after: kept,
            });
        }
    }
    content_losses(&mut before, &mut after, report);
    let mut unchecked: Vec<&str> = unchecked_before;
    unchecked.extend(unchecked_after);
    unchecked.sort_unstable();
    unchecked.dedup();
    if !unchecked.is_empty() {
        report.lost.push(unchecked_content(
            &unchecked.join(", "),
            &why_unsearched(&before, &after),
        ));
    }
    if had.contains_key("digital signatures") {
        report
            .notes
            .push("the digital signature will not match the changed file".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A part to zip: its name, its text, and its raw deflate stream, if
    /// any.
    type Part<'a> = (&'a str, &'a [u8], Option<&'a [u8]>);

    /// A zip package of `parts`: each stored, or with the raw deflate
    /// stream given (which must unpack to the text).
    fn zip_of(parts: &[Part<'_>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, text, deflated) in parts {
            let (method, body): (u16, &[u8]) = match deflated {
                Some(d) => (8, d),
                None => (0, text),
            };
            let mut crc = flate2::Crc::new();
            crc.update(text);
            let offset = out.len() as u32;
            let mut fields = Vec::new();
            fields.extend(20u16.to_le_bytes()); // version needed
            fields.extend(0u16.to_le_bytes()); // flags
            fields.extend(method.to_le_bytes());
            fields.extend(0u32.to_le_bytes()); // time, date
            fields.extend(crc.sum().to_le_bytes());
            fields.extend((body.len() as u32).to_le_bytes());
            fields.extend((text.len() as u32).to_le_bytes());
            fields.extend((name.len() as u16).to_le_bytes());
            fields.extend(0u16.to_le_bytes()); // extra
            out.extend(b"PK\x03\x04");
            out.extend(&fields);
            out.extend(name.as_bytes());
            out.extend(body);
            central.extend(b"PK\x01\x02");
            central.extend(20u16.to_le_bytes()); // version made by
            central.extend(&fields);
            central.extend([0u8; 10]); // comment, disk, attributes
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend(&central);
        out.extend(b"PK\x05\x06");
        out.extend([0u8; 4]);
        out.extend((parts.len() as u16).to_le_bytes());
        out.extend((parts.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(at.to_le_bytes());
        out.extend([0u8; 2]);
        out
    }

    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let parts: Vec<Part<'_>> = parts
            .iter()
            .map(|(n, t)| (*n, t.as_bytes(), None))
            .collect();
        zip_of(&parts)
    }

    #[test]
    fn content_lost_inside_rewritten_parts_is_reported() {
        let sheet = r#"<worksheet><sheetPr codeName="Sheet1"/><sheetData><row><c r="B2"><f t="dataTable" ref="B2:B3" r1="A1"/><v>3</v></c><c r="C2"><f>_xlfn.NEWFN(1)</f><v>7</v></c></row></sheetData><x:hyperlinks xmlns:x="m"><x:hyperlink ref="A1" display="Go &amp; see"/></x:hyperlinks></worksheet>"#;
        let source = package(&[
            (
                "xl/workbook.xml",
                r#"<workbook><fileSharing userName="Ann"/><workbookPr codeName="ThisWorkbook"/></workbook>"#,
            ),
            ("xl/worksheets/sheet1.xml", sheet),
            (
                "xl/sharedStrings.xml",
                "<sst><si><r><rPr><b/></rPr><t>a</t></r></si></sst>",
            ),
            (
                "xl/persons/person.xml",
                r#"<personList><person displayName="Ann" id="{1}" userId="a@x" providerId="AD"/></personList>"#,
            ),
        ]);
        // The same content, written differently: nothing is reported.
        let same = package(&[
            (
                "xl/workbook.xml",
                r#"<workbook><fileSharing userName='Ann'/><workbookPr date1904="0" codeName="ThisWorkbook"/></workbook>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                &sheet.replace("x:", "").replace("&amp;", "&#38;"),
            ),
            (
                "xl/sharedStrings.xml",
                "<sst><si><r><rPr><b/></rPr><t>a</t></r></si></sst>",
            ),
            (
                "xl/persons/person.xml",
                r#"<personList><person displayName="Ann" id="{9}" userId="a@x" providerId="AD"/></personList>"#,
            ),
        ]);
        let mut r = SaveReport::new(carrysheet_commands::engine::FileFormat::Xlsx);
        compare(&source, &same, &mut r);
        assert!(r.lost.is_empty(), "{:?}", r.lost);

        // Every part rewritten without it: each is reported, by name.
        let lossy = package(&[
            ("xl/workbook.xml", "<workbook><workbookPr/></workbook>"),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c r="B2"><v>3</v></c><c r="C2" t="e"><f>NEWFN(1)</f><v>#NAME?</v></c></row></sheetData><hyperlinks><hyperlink ref="A1" display="A1"/></hyperlinks></worksheet>"#,
            ),
            ("xl/sharedStrings.xml", "<sst><si><t>a</t></si></sst>"),
            (
                "xl/persons/person.xml",
                r#"<personList><person displayName="Ann" id="{1}" userId="Ann" providerId="None"/></personList>"#,
            ),
        ]);
        let mut r = SaveReport::new(carrysheet_commands::engine::FileFormat::Xlsx);
        compare(&source, &lossy, &mut r);
        let lost: Vec<&str> = r.lost.iter().map(|l| l.feature.as_str()).collect();
        for feature in [
            "file sharing settings",
            "workbook code name (VBA)",
            "sheet code names (VBA)",
            "what-if data tables",
            "hyperlink text and tips",
            "rich text in cells",
            "people of threaded comments (user ids)",
            "formula results (they would become #NAME?)",
            "newer Excel function names (_xlfn.)",
        ] {
            assert!(lost.contains(&feature), "{feature}: {lost:?}");
        }
    }

    #[test]
    fn a_package_too_large_to_search_needs_confirm_loss() {
        // Round 2, N4: what the gate could not search is a loss, not a note.
        let sheet = r#"<worksheet><sheetPr codeName="Sheet1"/><sheetData/></worksheet>"#;
        let parts = [
            ("xl/workbook.xml", "<workbook/>"),
            ("xl/worksheets/sheet1.xml", sheet),
        ];
        let source = package(&parts);
        let small = Caps {
            part: 16,
            total: 16,
        };
        let mut r = SaveReport::new(carrysheet_commands::engine::FileFormat::Xlsx);
        compare_within(&source, &source, &mut r, small);
        let lost: Vec<&str> = r.lost.iter().map(|l| l.feature.as_str()).collect();
        assert!(
            lost.iter()
                .any(|l| l.contains("content inside rewritten parts")),
            "{lost:?}"
        );
        assert!(lost.iter().any(|l| l.contains("not checked")), "{lost:?}");
        assert!(lost.iter().all(|l| !l.contains("slowly")), "{lost:?}");
        assert!(lost.iter().any(|l| l.contains("too large")), "{lost:?}");
        // With room to search, the same package loses nothing.
        let mut r = SaveReport::new(carrysheet_commands::engine::FileFormat::Xlsx);
        compare(&source, &source, &mut r);
        assert!(r.lost.is_empty(), "{:?}", r.lost);
    }

    /// Raw deflate of `text` in stored blocks, with `filler` bytes of
    /// empty stored blocks (5 bytes each, no output) after the first
    /// `split` bytes of output.
    fn with_empty_run(text: &[u8], split: usize, filler: usize) -> Vec<u8> {
        fn stored(out: &mut Vec<u8>, data: &[u8], last: bool) {
            let chunks: Vec<&[u8]> = data.chunks(65_535).collect();
            for (i, chunk) in chunks.iter().enumerate() {
                out.push(u8::from(last && i + 1 == chunks.len()));
                let n = chunk.len() as u16;
                out.extend(n.to_le_bytes());
                out.extend((!n).to_le_bytes());
                out.extend(*chunk);
            }
        }
        let mut out = Vec::new();
        stored(&mut out, &text[..split], false);
        for _ in 0..filler / 5 {
            out.extend(b"\x00\x00\x00\xff\xff");
        }
        stored(&mut out, &text[split..], true);
        out
    }

    #[test]
    fn a_long_run_of_empty_blocks_between_outputs_is_refused_and_named() {
        // Issue 80: a sheet part no relationship names (only this check
        // unpacks it) gives 192 KiB of output, then 40 KiB of empty
        // blocks, then the rest. Its total stays within what its output
        // earns (a quarter more input than output), so only the cap on a
        // run without output refuses it, far below the 64 MiB part cap.
        let mut sheet = b"<worksheet><sheetData/><!--".to_vec();
        sheet.resize(256 << 10, b'x');
        sheet.extend(b"--></worksheet>");
        let packed = with_empty_run(&sheet, 192 << 10, 40 << 10);
        assert!(packed.len() as u64 <= sheet.len() as u64 * 5 / 4);
        let workbook = b"<workbook/>".as_slice();
        let source = zip_of(&[
            ("xl/workbook.xml", workbook, None),
            (
                "xl/worksheets/sheet9.xml",
                sheet.as_slice(),
                Some(packed.as_slice()),
            ),
        ]);
        // Read whole, it is refused for its shape, and nothing else.
        let mut p = Package::open(&source).unwrap();
        let e = p.parts["xl/worksheets/sheet9.xml"].1;
        assert!(p.unpack(e).is_none());
        assert_eq!(
            p.unsearched.iter().copied().collect::<Vec<_>>(),
            [Unsearched::Slow]
        );

        let mut r = SaveReport::new(carrysheet_commands::engine::FileFormat::Xlsx);
        compare(&source, &source, &mut r);
        let lost: Vec<&str> = r.lost.iter().map(|l| l.feature.as_str()).collect();
        assert!(!lost.is_empty(), "{lost:?}");
        for l in &lost {
            assert!(l.contains("not checked"), "{l}");
            assert!(l.contains("unpacks far more slowly"), "{l}");
            assert!(!l.contains("too large"), "{l}");
        }

        // The same part without the run is searched, and loses nothing.
        let plain = zip_of(&[
            ("xl/workbook.xml", workbook, None),
            (
                "xl/worksheets/sheet9.xml",
                sheet.as_slice(),
                Some(with_empty_run(&sheet, 192 << 10, 0).as_slice()),
            ),
        ]);
        let mut r = SaveReport::new(carrysheet_commands::engine::FileFormat::Xlsx);
        compare(&plain, &plain, &mut r);
        assert!(r.lost.is_empty(), "{:?}", r.lost);
    }

    #[test]
    fn tags_are_read_with_local_names_and_decoded_values() {
        let mut seen = vec![];
        for_each_tag(
            br#"<?xml version="1.0"?><!-- <b x="1"/> --><a:t q='x>y' r="&lt;&#65;&#x42;"/><![CDATA[<c/>]]></a:t>"#,
            &mut |name, attrs| {
                seen.push(format!(
                    "{name}:{}",
                    attrs
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            },
        );
        assert_eq!(seen, vec!["t:q=x>y,r=<AB"]);
    }

    #[test]
    fn counting_needles() {
        assert_eq!(count(b"<a ><a ><b>", b"<a "), 2);
        assert_eq!(count(b"", b"<a "), 0);
        assert_eq!(count(b"aaaa", b"aa"), 2);
    }

    #[test]
    fn part_names_match_features() {
        let label = |name: &str| PART_FEATURES.iter().find(|(_, t)| t(name)).map(|(l, _)| *l);
        assert_eq!(label("xl/charts/chart1.xml"), Some("charts"));
        assert_eq!(label("xl/charts/_rels/chart1.xml.rels"), None);
        assert_eq!(label("xl/vbaproject.bin"), Some("VBA project"));
        assert_eq!(
            label("xl/drawings/vmldrawing1.vml"),
            Some("notes and form controls (VML)")
        );
        assert_eq!(label("xl/drawings/drawing1.xml"), Some("drawings"));
        assert_eq!(label("customxml/itemprops1.xml"), None);
        assert_eq!(label("xl/worksheets/sheet1.xml"), None);
    }
}
