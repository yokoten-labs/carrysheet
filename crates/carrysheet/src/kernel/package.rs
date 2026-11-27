//! Comparing two OOXML packages part by part, and counting the features a
//! save must keep (record 0003, point 3). The approach follows the kernel
//! bench's A5 check (research/kernel-bench on branch kernel-bench).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use crate::save::{Loss, MAX_LISTED_PARTS, PartCounts, SaveReport};
use carrysheet_zip::{Archive, DuplicateNames, Limits as ZipLimits};
use gridcraft_engine::core::limits as gc;

/// Most entries listed from one package (as GridCraft reads).
const MAX_ENTRIES: usize = 200_000;
/// Largest central directory read: 64 MiB, room for [`MAX_ENTRIES`]
/// entries with names of up to about 290 bytes (each entry is a 46-byte
/// header and its name). Excel's part names are far shorter.
const MAX_DIRECTORY: u64 = 64 << 20;
/// Largest part searched for features inside sheets and the workbook.
const MAX_SCANNED_PART: u64 = 64 << 20;
/// Most bytes searched in one package.
const MAX_SCANNED_TOTAL: u64 = 512 << 20;

/// Parts whose loss does not matter: caches Excel rebuilds.
const HARMLESS: &[&str] = &["xl/calcchain.xml"];

/// One part as the zip directory describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    index: usize,
    crc: u32,
    size: u64,
}

/// Why some of a package was not searched, as the loss text says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Unsearched {
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
    pub(crate) parts: BTreeMap<String, (String, Entry)>,
    /// Parts already refused by [`Package::unpack`], with why: each search
    /// of the package asks again, and must not pay for the same refusal
    /// twice.
    refused: BTreeMap<usize, Unsearched>,
    /// Why searches of this package stopped short, if any did.
    pub(crate) unsearched: BTreeSet<Unsearched>,
    caps: Caps,
}

/// How much of a package is searched: [`Caps::DEFAULT`] outside tests.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Caps {
    pub(crate) part: u64,
    pub(crate) total: u64,
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
    /// Lists a package's parts without unpacking them; when the bytes are
    /// not a zip the hardened reader opens, says why in plain words.
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Package<'a>, String> {
        let mut zip = Archive::open(Cursor::new(bytes), zip_limits()).map_err(unlisted)?;
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
        Ok(Package {
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
    pub(crate) fn unpack(&mut self, e: Entry) -> Option<Vec<u8>> {
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
pub(crate) fn for_each_tag(data: &[u8], f: &mut TagFn<'_>) {
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

pub(crate) fn count(hay: &[u8], needle: &[u8]) -> u64 {
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
pub(crate) const PART_FEATURES: &[PartFeature] = &[
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

/// Features that are macros: never run, kept when the save keeps them.
const MACRO_FEATURES: &[&str] = &["VBA project", "macro sheets (Excel 4)"];

/// Content types that declare macros, lowercased: a VBA project, and
/// Excel 4 macro sheets (`macrosheet` also matches `intlmacrosheet` and the
/// binary .xlsb type).
const MACRO_TYPES: &[(&str, &[u8])] = &[
    ("VBA project", b"vbaproject"),
    ("macro sheets (Excel 4)", b"macrosheet"),
];

/// What `overview` says about macros in a zip package (xlsx, xlsm, xlsb),
/// found by part name and by the content types in `[Content_Types].xml`:
/// Excel finds a VBA project by relationship and content type, so a
/// renamed part is still found. Only that one part is unpacked, and nothing
/// in the text comes from the file. `None` only when both were read and
/// neither names a macro; when either cannot be read, says that macros were
/// not checked. `kept` is false when a save cannot keep them (an .xlsb is
/// imported).
pub(crate) fn macros(bytes: &[u8], kept: bool) -> Option<String> {
    let mut p = match Package::open(bytes) {
        Ok(p) => p,
        Err(why) => {
            return Some(format!(
                "not checked: Carrysheet cannot list the file's parts ({why}); it never runs macros"
            ));
        }
    };
    let mut found: Vec<&str> = PART_FEATURES
        .iter()
        .filter(|(label, test)| MACRO_FEATURES.contains(label) && p.names().any(test))
        .map(|(label, _)| *label)
        .collect();
    let mut typed = [false; MACRO_TYPES.len()];
    // A plain byte search cannot see through XML character references or a
    // UTF-16 encoding, so content types written that way are not checked.
    let mut encoded = false;
    let read = p.each(|n| n == "[content_types].xml", &mut |data| {
        let low = data.to_ascii_lowercase();
        for (seen, (_, needle)) in typed.iter_mut().zip(MACRO_TYPES) {
            *seen |= count(&low, needle) > 0;
        }
        encoded |= count(data, b"&#") > 0
            || data.contains(&0)
            || data.starts_with(&[0xFF, 0xFE])
            || data.starts_with(&[0xFE, 0xFF]);
    });
    for (seen, (label, _)) in typed.iter().zip(MACRO_TYPES) {
        if *seen && !found.contains(label) {
            found.push(label);
        }
    }
    if found.is_empty() && (!read || encoded) {
        return Some(
            "not checked: Carrysheet cannot read the file's content types; it never runs macros"
                .to_owned(),
        );
    }
    let save = if kept {
        "keeps them on save where the format can hold them, and says before a save that would drop them"
    } else {
        "a save may not keep them (what the import skips is not listed)"
    };
    (!found.is_empty()).then(|| {
        format!(
            "has macros ({}): Carrysheet never runs them; {save}",
            found.join(", ")
        )
    })
}

/// Feature counts of a package, for reports when the output is not one.
/// When its parts cannot be listed, why.
pub(crate) fn source_features(bytes: &[u8]) -> Result<BTreeMap<&'static str, u64>, String> {
    Package::open(bytes).map(|mut p| features(&mut p).0)
}

/// Why the zip reader would not list a package's parts, in plain words
/// (its texts hold nothing from the file).
fn unlisted(e: carrysheet_zip::Error) -> String {
    match e {
        carrysheet_zip::Error::LimitExceeded {
            what: "zip entries",
            ..
        } => format!("it has more than {MAX_ENTRIES} parts"),
        carrysheet_zip::Error::LimitExceeded {
            what: "bytes of the central directory",
            ..
        } => format!(
            "its list of parts is larger than {} MiB",
            MAX_DIRECTORY >> 20
        ),
        carrysheet_zip::Error::Unsupported(_) => {
            format!("it uses zip features Excel does not write ({e})")
        }
        e => format!("its zip structure cannot be read ({e})"),
    }
}

/// The loss reported when a package cannot be compared: the save must not
/// pass as losing nothing (record 0003). It says what was not checked,
/// why, and what to do.
pub(crate) fn unchecked_loss(why: &str) -> Loss {
    Loss {
        feature: format!(
            "anything the file holds besides cell values (not checked: its parts could not be listed because {why}; open and save the file in Excel to repair it, or pass confirm_loss to save without the check)"
        ),
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

pub(crate) fn compare_within(source: &[u8], output: &[u8], report: &mut SaveReport, caps: Caps) {
    let (mut before, mut after) = match (Package::open(source), Package::open(output)) {
        (Ok(b), Ok(a)) => (b, a),
        (Err(why), _) => {
            report.lost.push(unchecked_loss(&why));
            return;
        }
        (_, Err(why)) => {
            report
                .lost
                .push(unchecked_loss(&format!("in the file to be written, {why}")));
            return;
        }
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
                    crate::save::part_name(shown)
                ));
            }
            None => {
                counts.dropped += 1;
                if report.dropped_parts.len() < MAX_LISTED_PARTS {
                    report.dropped_parts.push(crate::save::part_name(shown));
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
