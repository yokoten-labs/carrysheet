//! The save-loss report (record 0003, point 3; record 0010, decision 11):
//! before a save, the opened package and the one to be written are compared
//! part by part from their zip directories, so no part is lost silently.
//! Features are found by part name. Only `[Content_Types].xml` is unpacked,
//! for [`macros`].

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use zip::{ZipArchive, result::ZipError};

use super::{Loss, MAX_LISTED_PARTS, SaveReport};
use crate::text::quote;

/// Most parts listed from one package (as GridCraft reads).
const MAX_ENTRIES: usize = 200_000;
/// The largest `[Content_Types].xml` unpacked.
const MAX_CONTENT_TYPES: u64 = 4 << 20;
/// Parts whose loss does not matter: caches Excel rebuilds.
const HARMLESS: &[&str] = &["xl/calcchain.xml"];

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

/// One part as the zip directory describes it: index, size and packed size.
type Entry = (usize, u64, u64);

/// A package's parts by normalized name (lower case, no leading `/`).
struct Package<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    parts: BTreeMap<String, (String, Entry)>,
}

impl<'a> Package<'a> {
    /// Lists the parts without unpacking them, with the zip crate GridCraft
    /// reads with; when it cannot, says why in plain words.
    fn open(bytes: &'a [u8]) -> Result<Package<'a>, String> {
        let zip = ZipArchive::new(Cursor::new(bytes)).map_err(|e| match e {
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
            let entry = (i, f.size(), f.compressed_size());
            if !f.is_dir() {
                let normalized = name.replace('\\', "/");
                let normalized = normalized.trim_start_matches('/').to_ascii_lowercase();
                parts.entry(normalized).or_insert((name, entry));
            }
        }
        Ok(Package { zip, parts })
    }

    /// The bytes of `[Content_Types].xml` (none when it is absent), or
    /// `None` when its sizes are outside the bounds or it cannot be unpacked.
    fn content_types(&mut self) -> Option<Vec<u8>> {
        let Some((_, (index, size, packed))) = self.parts.get("[content_types].xml") else {
            return Some(Vec::new());
        };
        // Far more packed bytes than unpacked ones is how padding looks (issue 80).
        if *size > MAX_CONTENT_TYPES || *packed > size + size / 4 + 1024 {
            return None;
        }
        let (index, size) = (*index, *size);
        let mut data = Vec::new();
        let part = self.zip.by_index(index).ok()?;
        part.take(size).read_to_end(&mut data).ok()?;
        Some(data)
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
        let excluded = patterns.clone().any(|p| p.strip_prefix('-').is_some_and(matches));
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
    let types = [("VBA project", "vbaproject"), ("macro sheets (Excel 4)", "macrosheet")];
    let mut found: Vec<&str> =
        types.iter().map(|t| t.0).filter(|t| by_name.contains_key(t)).collect();
    let data = p.content_types();
    let low = data.as_deref().unwrap_or_default().to_ascii_lowercase();
    for (label, kind) in types {
        if low.windows(kind.len()).any(|w| w == kind.as_bytes()) && !found.contains(&label) {
            found.push(label);
        }
    }
    // A byte search cannot see through character references or UTF-16.
    let unreadable = low.windows(2).any(|w| w == b"&#") || low.contains(&0);
    if found.is_empty() && (data.is_none() || unreadable) {
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

/// The features found by part name, for a save to csv; when the parts
/// cannot be listed, why.
pub(crate) fn source_features(bytes: &[u8]) -> Vec<Loss> {
    match Package::open(bytes) {
        Ok(p) => {
            let features = p.part_features().into_iter();
            features.map(|(f, n)| Loss::all(f, n)).collect()
        }
        Err(why) => vec![unchecked_loss(&why)],
    }
}

/// The loss when a package cannot be compared: the save must not pass as
/// losing nothing (record 0003). It says what, why, and what to do.
pub(crate) fn unchecked_loss(why: &str) -> Loss {
    let feature = format!(
        "anything the file holds besides cell values (not checked: its parts could not be listed because {why}; open and save the file in Excel to repair it, or pass confirm_loss to save without the check)"
    );
    Loss::all(feature, 1)
}

/// Compares the opened package with the one about to be written.
pub(crate) fn compare(source: &[u8], output: &[u8], report: &mut SaveReport) {
    let (before, after) = match (Package::open(source), Package::open(output)) {
        (Ok(b), Ok(a)) => (b, a),
        (Err(why), _) => return report.lost.push(unchecked_loss(&why)),
        (_, Err(why)) => {
            let why = format!("in the file to be written, {why}");
            return report.lost.push(unchecked_loss(&why));
        }
    };
    let dropped = before.parts.iter().filter(|(name, _)| !after.parts.contains_key(*name));
    for (name, (shown, _)) in dropped {
        if HARMLESS.contains(&name.as_str()) {
            let note = format!("{} is not written; Excel rebuilds it", quote(shown, 64));
            report.notes.push(note);
        } else if report.dropped_parts.len() < MAX_LISTED_PARTS {
            report.dropped_parts.push(quote(shown, 64));
        }
    }

    // By feature: how many the source has, and how many of those the
    // output keeps.
    let (had, has) = (before.part_features(), after.part_features());
    for (feature, n) in &had {
        let kept = has.get(feature).map_or(0, |k| *k.min(n));
        if kept < *n {
            report.lost.push(Loss { feature: (*feature).to_owned(), before: *n, after: kept });
        }
    }
    if had.contains_key("digital signatures") {
        let note = "the digital signature will not match the changed file";
        report.notes.push(note.into());
    }
}
