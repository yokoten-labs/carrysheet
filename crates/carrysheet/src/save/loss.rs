//! The save-loss report (record 0003, point 3; record 0010, decision 2):
//! before a save, the opened package and the one to be written are compared
//! part by part, so nothing is lost silently. Features are found by part
//! name; then watched elements inside parts are compared (review H4).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::rc::Rc;

use gridcraft_engine::core::limits as gc;
use zip::{ZipArchive, result::ZipError};

use super::{Loss, MAX_LISTED_PARTS, PartCounts, SaveReport, part_name};

/// Most parts listed from one package (as GridCraft reads).
const MAX_ENTRIES: usize = 200_000;
/// Bytes searched inside: per part, and per package.
const CAPS: (u64, u64) = (64 << 20, 512 << 20);
/// Packed bytes a part may read in a row without unpacking anything (issue
/// 80): real deflate gives output within each block, padding does not.
const MAX_IDLE_READ: u64 = 16 << 10;
/// Parts whose loss does not matter: caches Excel rebuilds.
const HARMLESS: &[&str] = &["xl/calcchain.xml"];

// Why some of a package was not searched, as the loss text says it.
pub(crate) const TOO_LARGE: &str = "the file is too large to search";
pub(crate) const SLOW: &str = "a part was refused because it unpacks far more slowly than real files do, as files built to waste time are";
const UNREADABLE: &str = "a part could not be unpacked; it may be damaged";
const OUT_OF_TIME: &str = "the command's time limit ran out while unpacking parts";

/// Features found by part name, one per line: the label, then patterns.
/// `d/s*` is a part in `d` (not below) whose name starts with `s`; `d/**`
/// is anything below `d`; `-p` excludes; other patterns are whole names.
pub(crate) const PART_FEATURES: &str = "\
VBA project: xl/vbaproject.bin
charts: xl/charts/chart*
chart sheets: xl/chartsheets/sheet*
pivot tables: xl/pivottables/pivottable*
pivot caches: xl/pivotcache/pivotcachedefinition*
slicers: xl/slicers/* xl/slicercaches/*
timelines: xl/timelines/* xl/timelinecaches/*
comments: xl/comments*
threaded comments: xl/threadedcomments/*
notes and form controls (VML): xl/drawings/vmldrawing*
drawings: xl/drawings/drawing*
images: xl/media/*
tables: xl/tables/table*
external links: xl/externallinks/externallink*
data connections: xl/connections.xml
query tables: xl/querytables/*
embedded objects: xl/embeddings/*
ActiveX controls: xl/activex/*
custom XML: customxml/item* -customxml/itemprops*
custom document properties: docprops/custom.xml
macro sheets (Excel 4): xl/macrosheets/*
dialog sheets: xl/dialogsheets/*
data model: xl/model/**
cell images and rich data: xl/richdata/*
web extensions: xl/webextensions/*
digital signatures: _xmlsignatures/**";

/// Packed bytes read so far, and the most a part read may reach.
#[derive(Default)]
struct Allowance {
    read: Cell<u64>,
    until: Cell<Option<u64>>,
}

/// The package bytes, read within an [`Allowance`].
struct Counted<'a>(Cursor<&'a [u8]>, Rc<Allowance>);

impl Read for Counted<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let (read, until) = (self.1.read.get(), self.1.until.get());
        let left = until.map_or(u64::MAX, |u| u.saturating_sub(read));
        if left == 0 && !buf.is_empty() {
            return Err(std::io::Error::other(
                "the part reads far more than it unpacks",
            ));
        }
        let n = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let got = self.0.read(buf.get_mut(..n).unwrap_or_default())?;
        self.1.read.set(read + got as u64);
        Ok(got)
    }
}

impl Seek for Counted<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

/// One part as the zip directory describes it: index, CRC-32, size, and
/// packed size.
pub(crate) type Entry = (usize, u32, u64, u64);

/// A package's parts by normalized name (lower case, no leading `/`).
pub(crate) struct Package<'a> {
    zip: ZipArchive<Counted<'a>>,
    allowance: Rc<Allowance>,
    pub(crate) parts: BTreeMap<String, (String, Entry)>,
    /// Why searches stopped short, if any did.
    pub(crate) unsearched: BTreeSet<&'static str>,
    caps: (u64, u64),
}

impl<'a> Package<'a> {
    /// Lists the parts without unpacking them, with the zip crate GridCraft
    /// reads with; when it cannot, says why in plain words.
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Package<'a>, String> {
        let allowance = Rc::new(Allowance::default());
        let reader = Counted(Cursor::new(bytes), Rc::clone(&allowance));
        let zip = ZipArchive::new(reader).map_err(|e| match e {
            ZipError::UnsupportedArchive(_) => {
                format!("it uses zip features Excel does not write ({e})")
            }
            e => format!("its zip structure cannot be read (the zip is damaged: {e})"),
        })?;
        if zip.len() > MAX_ENTRIES {
            return Err(format!("it has more than {MAX_ENTRIES} parts"));
        }
        let mut parts = BTreeMap::new();
        for (i, f) in (0..zip.len()).filter_map(|i| Some((i, zip.by_index_data(i).ok()?))) {
            let name = String::from_utf8_lossy(f.name_raw()).into_owned();
            let entry = (i, f.crc32(), f.size(), f.compressed_size());
            if !f.is_dir() {
                parts.entry(normalize(&name)).or_insert((name, entry));
            }
        }
        let unsearched = BTreeSet::new();
        Ok(Package {
            zip,
            allowance,
            parts,
            unsearched,
            caps: CAPS,
        })
    }

    /// The bytes of one part, or why it was not unpacked.
    pub(crate) fn unpack(&mut self, entry: Entry) -> Result<Vec<u8>, &'static str> {
        let (index, _, size, packed) = entry;
        if size > self.caps.0 {
            return Err(TOO_LARGE);
        }
        // Far more packed bytes than unpacked ones is how padding looks.
        if packed > size + size / 4 + 1024 {
            return Err(SLOW);
        }
        let a = Rc::clone(&self.allowance);
        let mut part = self.zip.by_index(index).map_err(|_| UNREADABLE)?;
        let (start, mut out, mut step) = (a.read.get(), Vec::new(), [0u8; 16 << 10]);
        let result = loop {
            if !gc::check_time() {
                break Err(OUT_OF_TIME);
            }
            // Each step may read a little more; in all, a quarter more
            // than came out.
            let made = out.len() as u64;
            let room = a.read.get().min(start + made + made / 4);
            a.until.set(Some(MAX_IDLE_READ + room));
            match part.read(&mut step) {
                Ok(0) => break Ok(out),
                Ok(n) if made + n as u64 > self.caps.0 => break Err(TOO_LARGE),
                Ok(n) => out.extend_from_slice(step.get(..n).unwrap_or_default()),
                Err(_) if a.until.get() == Some(a.read.get()) => break Err(SLOW),
                Err(_) => break Err(UNREADABLE),
            }
        };
        a.until.set(None);
        result
    }

    /// Calls `f` with the name and bytes of each part `pick` selects,
    /// within the caps; `false` when one was not searched.
    fn each(&mut self, pick: impl Fn(&str) -> bool, f: &mut dyn FnMut(&str, &[u8])) -> bool {
        let picked: Vec<(String, Entry)> = self
            .parts
            .iter()
            .filter(|(n, _)| pick(n))
            .map(|(n, (_, e))| (n.clone(), *e))
            .collect();
        let (mut total, cap) = (0u64, self.caps.1);
        for (name, entry) in picked {
            let fits = |data: Vec<u8>| match total + data.len() as u64 {
                sum if sum > cap => Err(TOO_LARGE),
                _ => Ok(data),
            };
            match self.unpack(entry).and_then(fits) {
                Ok(data) => {
                    total += data.len() as u64;
                    f(&name, &data);
                }
                Err(why) => {
                    self.unsearched.insert(why);
                    return false;
                }
            }
        }
        true
    }

    /// Counts of the features found by part name.
    fn part_features(&self) -> BTreeMap<&'static str, u64> {
        let mut found = BTreeMap::new();
        for label in self.parts.keys().filter_map(|n| kind(PART_FEATURES, n)) {
            *found.entry(label).or_default() += 1;
        }
        found
    }
}

fn normalize(name: &str) -> String {
    let name = name.replace('\\', "/");
    name.trim_start_matches('/').to_ascii_lowercase()
}

/// The label of the first line of `table` (`label: patterns`, as in
/// [`PART_FEATURES`]) whose patterns match `name`.
pub(crate) fn kind(table: &'static str, name: &str) -> Option<&'static str> {
    let matches = |pattern: &str| match pattern.strip_suffix('*') {
        Some(dir) if dir.ends_with("/*") => name.starts_with(dir.trim_end_matches('*')),
        Some(start) => {
            let (dir, stem) = start.rsplit_once('/').unwrap_or(("", start));
            let rest = name.strip_prefix(dir).and_then(|r| r.strip_prefix('/'));
            rest.is_some_and(|r| r.starts_with(stem) && !r.contains('/'))
        }
        None => name == pattern,
    };
    table.lines().find_map(|line| {
        let (label, patterns) = line.split_once(": ")?;
        let mut patterns = patterns.split(' ');
        let excluded = patterns
            .clone()
            .any(|p| p.strip_prefix('-').is_some_and(matches));
        (!excluded && patterns.any(matches)).then_some(label)
    })
}

/// What `overview` says about macros in a zip package (xlsx, xlsm, xlsb):
/// found by part name, and by content type in `[Content_Types].xml`, as
/// Excel finds a renamed VBA project. Nothing in the text comes from the
/// file. `None` only when both were read and neither names a macro. `kept`
/// is false when a save cannot keep them (an .xlsb is imported).
pub(crate) fn macros(bytes: &[u8], kept: bool) -> Option<String> {
    let mut p = match Package::open(bytes) {
        Ok(p) => p,
        Err(why) => {
            return Some(format!(
                "not checked: Carrysheet cannot list the file's parts ({why}); it never runs macros"
            ));
        }
    };
    let by_name = p.part_features();
    // Content types, lower case: `macrosheet` also matches `intlmacrosheet`.
    let types = [
        ("VBA project", "vbaproject"),
        ("macro sheets (Excel 4)", "macrosheet"),
    ];
    let mut found: Vec<&str> = types
        .iter()
        .map(|t| t.0)
        .filter(|t| by_name.contains_key(t))
        .collect();
    // A byte search cannot see through character references or UTF-16.
    let mut unreadable = false;
    let read = p.each(|n| n == "[content_types].xml", &mut |_, data| {
        let low = data.to_ascii_lowercase();
        for (label, kind) in types {
            if count(&low, kind.as_bytes()) > 0 && !found.contains(&label) {
                found.push(label);
            }
        }
        unreadable |= count(data, b"&#") > 0 || data.contains(&0);
    });
    if found.is_empty() && (!read || unreadable) {
        let text =
            "not checked: Carrysheet cannot read the file's content types; it never runs macros";
        return Some(text.into());
    }
    let save = if kept {
        "keeps them on save where the format can hold them, and says before a save that would drop them"
    } else {
        "a save may not keep them (what the import skips is not listed)"
    };
    let found = found.join(", ");
    (!found.is_empty()).then(|| format!("has macros ({found}): Carrysheet never runs them; {save}"))
}

/// Feature counts of a package, for a save to csv; when its parts cannot
/// be listed, why. Counts in parts that could not be searched are left out.
pub(crate) fn source_features(bytes: &[u8]) -> Result<BTreeMap<&'static str, u64>, String> {
    let mut p = Package::open(bytes)?;
    let mut features = p.part_features();
    let counted: Vec<&str> = WATCHED
        .lines()
        .filter_map(|l| Some(l.split_once(" # : ")?.1))
        .collect();
    for ((feature, _), n) in inside(&mut p).map(|i| i.elements).unwrap_or_default() {
        if counted.contains(&feature) {
            *features.entry(feature).or_default() += n;
        }
    }
    Ok(features)
}

/// The loss when a package cannot be compared: the save must not pass as
/// losing nothing (record 0003). It says what, why, and what to do.
pub(crate) fn unchecked_loss(why: &str) -> Loss {
    Loss {
        feature: format!(
            "anything the file holds besides cell values (not checked: its parts could not be listed because {why}; open and save the file in Excel to repair it, or pass confirm_loss to save without the check)"
        ),
        before: 1,
        after: 0,
    }
}

/// Compares the opened package with the one about to be written.
pub(crate) fn compare(source: &[u8], output: &[u8], report: &mut SaveReport) {
    compare_within(source, output, report, CAPS);
}

pub(crate) fn compare_within(
    source: &[u8],
    output: &[u8],
    report: &mut SaveReport,
    caps: (u64, u64),
) {
    let (mut before, mut after) = match (Package::open(source), Package::open(output)) {
        (Ok(b), Ok(a)) => (b, a),
        (Err(why), _) => return report.lost.push(unchecked_loss(&why)),
        (_, Err(why)) => {
            let why = format!("in the file to be written, {why}");
            return report.lost.push(unchecked_loss(&why));
        }
    };
    (before.caps, after.caps) = (caps, caps);
    let mut counts = PartCounts::default();
    for (name, (shown, e)) in &before.parts {
        match after.parts.get(name) {
            Some((_, a)) if (a.1, a.2) == (e.1, e.2) => counts.identical += 1,
            Some(_) => counts.rewritten += 1,
            None if HARMLESS.contains(&name.as_str()) => {
                counts.dropped += 1;
                let note = format!("{} is not written; Excel rebuilds it", part_name(shown));
                report.notes.push(note);
            }
            None => {
                counts.dropped += 1;
                if report.dropped_parts.len() < MAX_LISTED_PARTS {
                    report.dropped_parts.push(part_name(shown));
                }
            }
        }
    }
    let added = after
        .parts
        .keys()
        .filter(|k| !before.parts.contains_key(*k));
    counts.added = added.count() as u64;
    report.parts = Some(counts);

    // By feature: how many the source has, and how many of those the
    // output keeps.
    let (had, has) = (before.part_features(), after.part_features());
    let mut lost: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for (feature, n) in &had {
        lost.insert(feature, (*n, has.get(feature).map_or(0, |k| *k.min(n))));
    }
    let mut not_checked = None;
    if let (Some(was), Some(now)) = (inside(&mut before), inside(&mut after)) {
        for (key, n) in &was.elements {
            let e = lost.entry(key.0).or_default();
            e.0 += n;
            e.1 += now.elements.get(key).map_or(0, |k| *k.min(n));
        }
        // Formulas GridCraft cannot compute must keep their saved values.
        let ([errors, newer], [errors_now, newer_now]) = (was.names, now.names);
        let gained = errors_now.saturating_sub(errors);
        lost.insert("formula results (they would become #NAME?)", (gained, 0));
        lost.insert("newer Excel function names (_xlfn.)", (newer, newer_now));
    } else {
        // Not searched: a loss, so the save needs confirm_loss (round 2,
        // N4), with the real reasons (issue 80).
        let why: Vec<&str> = before
            .unsearched
            .union(&after.unsearched)
            .copied()
            .collect();
        let why = if why.is_empty() {
            TOO_LARGE.to_owned()
        } else {
            why.join("; ")
        };
        let feature = format!("content inside rewritten parts (not checked: {why})");
        not_checked = Some(Loss {
            feature,
            before: 1,
            after: 0,
        });
    }
    for (feature, (had, kept)) in lost.into_iter().filter(|(_, (b, a))| a < b) {
        let feature = feature.to_owned();
        report.lost.push(Loss {
            feature,
            before: had,
            after: kept,
        });
    }
    report.lost.extend(not_checked);
    if had.contains_key("digital signatures") {
        let note = "the digital signature will not match the changed file";
        report.notes.push(note.into());
    }
}

// ------------------------------------------------- content inside parts
//
// Part names miss content lost inside a part GridCraft rewrites (review
// H4). Both packages' watched elements are compared by the attributes that
// carry their meaning. Record 0010, decision 11 may cut the `#` lines.

/// The parts searched inside, as in [`PART_FEATURES`].
const SEARCHED: &str = "\
sheet: xl/worksheets/sheet*
book: xl/workbook.xml
strings: xl/sharedstrings.xml
notes: xl/comments*
people: xl/persons/*";

/// Watched elements: part, local name, the attributes compared (`*`: all;
/// `-`: none; `#`: none, and listed for a save to csv; `k=v` must match),
/// then the feature.
const WATCHED: &str = "\
book fileSharing * : file sharing settings
book workbookPr codeName : workbook code name (VBA)
sheet sheetPr codeName : sheet code names (VBA)
sheet f t=dataTable ref dt2D dtr r1 r2 : what-if data tables
sheet scenario name count user comment : scenarios
sheet inputCells r val : scenarios
sheet autoFilter ref : AutoFilter settings
sheet filter val : AutoFilter settings
sheet customFilter val : AutoFilter settings
sheet dynamicFilter type : AutoFilter settings
sheet dateGroupItem * : AutoFilter settings
sheet sortCondition ref descending : AutoFilter sort
sheet phoneticPr * : phonetic settings
sheet ignoredError * : ignored error flags
sheet hyperlink ref display tooltip : hyperlink text and tips
strings rPr - : rich text in cells
strings rPh * : phonetic guides
notes rPr - : formatted note text
people person displayName userId providerId : people of threaded comments (user ids)
sheet conditionalFormatting # : conditional formatting
sheet dataValidation # : data validation
sheet sparklineGroup # : sparklines
sheet hyperlink # : hyperlinks
sheet mergeCell # : merged cells
book definedName # : defined names";

/// What a package holds inside its parts: watched elements as (feature,
/// signature) counts; in sheets, `#NAME?` values and `_xlfn.` names.
#[derive(Default)]
struct Inside {
    elements: BTreeMap<(&'static str, String), u64>,
    names: [u64; 2],
}

/// `None` when a part was not searched (why is in `p.unsearched`).
fn inside(p: &mut Package<'_>) -> Option<Inside> {
    let watched: Vec<(Vec<&str>, &'static str)> = WATCHED
        .lines()
        .filter_map(|line| line.split_once(" : "))
        .map(|(spec, feature)| (spec.split(' ').collect(), feature))
        .collect();
    let mut found = Inside::default();
    let complete = p.each(|n| kind(SEARCHED, n).is_some(), &mut |name, data| {
        let part = kind(SEARCHED, name);
        for_each_tag(data, &mut |tag, attrs| {
            for (spec, feature) in &watched {
                if let [part_kind, element, keys @ ..] = spec.as_slice()
                    && Some(*part_kind) == part
                    && *element == tag
                    && let Some(sig) = signature(attrs, keys)
                {
                    *found.elements.entry((*feature, sig)).or_default() += 1;
                }
            }
        });
        if part == Some("sheet") {
            found.names[0] += count(data, b"<v>#NAME?</v>");
            found.names[1] += count(data, b"_xlfn.");
        }
    });
    complete.then_some(found)
}

fn signature(attrs: &Attributes<'_>, keys: &[&str]) -> Option<String> {
    let mut all: Vec<String> = attrs.iter().map(|(k, v)| format!("{k}={v}")).collect();
    all.sort_unstable();
    if keys
        .iter()
        .any(|key| key.contains('=') && !all.iter().any(|a| a == key))
    {
        return None;
    }
    if keys != ["*"] {
        all.retain(|a| {
            keys.iter()
                .any(|key| a.split_once('=').is_some_and(|(k, _)| k == *key))
        });
    }
    Some(all.join(" "))
}

/// A tag's attributes: local names, values with entities decoded.
type Attributes<'a> = [(&'a str, String)];

/// Calls `f` with the local name and attributes (local names, entities
/// decoded) of each start tag. A light scanner, not a parser: GridCraft
/// has read both packages, so they are well formed.
pub(crate) fn for_each_tag(data: &[u8], f: &mut dyn FnMut(&str, &Attributes<'_>)) {
    let text = String::from_utf8_lossy(data);
    let (mut rest, mut attrs) = (text.as_ref(), Vec::new());
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let skip = [("!--", "-->"), ("![CDATA[", "]]>")];
        if let Some((_, end)) = skip.iter().find(|s| rest.starts_with(s.0)) {
            rest = rest.find(end).map_or("", |e| &rest[e + end.len()..]);
            continue;
        }
        if rest.starts_with(['/', '?', '!']) {
            continue;
        }
        // The tag runs to the first `>` outside quotes.
        let mut quote = None;
        let end = rest.find(|c| {
            quote = match quote {
                Some(q) => Some(q).filter(|q| *q != c),
                None => Some(c).filter(|c| *c == '"' || *c == '\''),
            };
            quote.is_none() && c == '>'
        });
        let (tag, after_tag) = rest.split_at(end.unwrap_or(rest.len()));
        let tag = tag.trim_end_matches('/');
        let (name, mut a) = tag.split_at(tag.find(char::is_whitespace).unwrap_or(tag.len()));
        attrs.clear();
        while let Some((key, value)) = a.split_once('=') {
            let value = value.trim_start();
            let Some(q) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                break;
            };
            let (value, next) = value[1..].split_once(q).unwrap_or((&value[1..], ""));
            if !key.trim().starts_with("xmlns") {
                attrs.push((local(key.trim()), unescape(value)));
            }
            a = next;
        }
        f(local(name), &attrs);
        rest = after_tag;
    }
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn unescape(v: &str) -> String {
    let (mut out, mut rest) = (String::with_capacity(v.len()), v);
    while let Some((head, tail)) = rest.split_once('&') {
        out.push_str(head);
        let (entity, after) = tail
            .split_once(';')
            .filter(|(e, _)| e.len() <= 10)
            .unwrap_or(("", tail));
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e => (e
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok()))
            .or_else(|| e.strip_prefix('#').and_then(|d| d.parse().ok()))
            .and_then(char::from_u32),
        };
        out.push(c.unwrap_or('&'));
        rest = if c.is_some() { after } else { tail };
    }
    out.push_str(rest);
    out
}

pub(crate) fn count(hay: &[u8], needle: &[u8]) -> u64 {
    let (Some(&first), mut n, mut i) = (needle.first(), 0, 0) else {
        return 0;
    };
    while let Some(at) = hay
        .get(i..)
        .and_then(|r| r.iter().position(|&b| b == first))
    {
        let found = hay[i + at..].starts_with(needle);
        n += u64::from(found);
        i += at + if found { needle.len() } else { 1 };
    }
    n
}
