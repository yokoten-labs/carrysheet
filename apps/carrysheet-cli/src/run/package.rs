//! Finding the VBA project inside a workbook file, treating the file as
//! hostile: every size, count and step is bounded, and nothing is
//! extracted to disk.
//!
//! - `.xls`: an OLE2 compound file; the project is its `_VBA_PROJECT_CUR`
//!   storage ([`VbaPart::from_xls`]). A `vbaProject.bin` saved on its own
//!   is a compound file too, and is read as one.
//! - `.xlsm`, `.xlsb`, `.xltm`, `.xlam` (and `.xlsx`, which should have
//!   none): a zip package. The project Excel loads is the one the
//!   workbook's relationships name. Every entry is checked too: a part
//!   whose content type is a VBA project, or a compound file with a `VBA`
//!   storage, is a candidate. "No macros" is said only when there is none,
//!   and more than one candidate, or relationships that disagree with the
//!   zip, are reported as warnings with every candidate listed.
//!
//! The file is recognised by its first bytes, not its name.
//!
//! Names and paths from the file appear in messages only through
//! [`text::name`]; `fix` texts never hold them.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::{Duration, Instant};

use carrysheet_commands::paths::display_path;
use carrysheet_commands::{CommandError, ErrorCode};
use carrysheet_vba::{Limits, VbaError, VbaPart};
use carrysheet_zip::{Archive, DuplicateNames, Error as ZipError, Limits as ZipLimits};

use super::text;
use super::xml::{self, XmlError};

/// The largest `.xls` file read into memory (the VBA reader's own limit).
pub const MAX_XLS_BYTES: u64 = 512 << 20;
/// The most entries a zip package may have. Real workbooks have tens to a
/// few thousand (one per sheet, drawing, image).
pub const MAX_ZIP_ENTRIES: usize = 50_000;
/// The largest `vbaProject.bin`, uncompressed. Real projects are well
/// under 10 MB.
pub const MAX_VBA_PART_BYTES: u64 = 64 << 20;
/// All compound-file entries inflated while looking for VBA projects,
/// together. They are not all held at once: at most two are in memory.
pub const MAX_CANDIDATE_BYTES: u64 = 256 << 20;
/// The largest relationships or content-types part, uncompressed.
pub const MAX_XML_PART_BYTES: u64 = 4 << 20;
/// Elements of one kind read from one XML part.
const MAX_XML_ELEMENTS: usize = MAX_ZIP_ENTRIES;
/// VBA project candidates listed; more are counted.
pub const MAX_CANDIDATES: usize = 8;
/// Compressed bytes the scan of one package may read. Over Apache POI's
/// 350 test packages the scan read 4.7 KB at the median, 59 KB at the
/// 99th percentile and 1.8 MB at most; 128 MiB is 70 times that, and
/// still holds two VBA projects at their 64 MB limit.
pub const MAX_READ_BYTES: u64 = 128 << 20;
/// Time the scan of one package may take. Over the same corpus the
/// slowest took 17 ms. Deflate data can be built to cost time without
/// output; the zip reader stops most of it, and this stops the rest.
pub const SCAN_SECONDS: u64 = 10;

const OLE2_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const VBA_CONTENT_TYPE: &str = "application/vnd.ms-office.vbaProject";
/// The namespaces of `[Content_Types].xml` and of relationships parts
/// (Open Packaging Conventions; the same in strict documents).
const CONTENT_TYPES_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/content-types";
const RELATIONSHIPS_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships";
const VBA_RELATIONSHIP: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
const OFFICE_DOCUMENT_RELATIONSHIPS: &[&str] = &[
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
    "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument",
];

/// How a VBA project part was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundBy {
    /// The workbook's relationships name it: the one Excel loads.
    Relationship,
    /// `[Content_Types].xml` gives it the VBA project content type.
    ContentType,
    /// It is a compound file with a `VBA` storage, whatever its name.
    Contents,
}

impl FoundBy {
    pub fn as_str(self) -> &'static str {
        match self {
            FoundBy::Relationship => "relationship",
            FoundBy::ContentType => "content_type",
            FoundBy::Contents => "contents",
        }
    }
}

/// A part that holds, or claims to hold, a VBA project.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The zip entry name, raw. Show it with [`text::name`].
    pub name: String,
    pub found_by: Vec<FoundBy>,
    /// Modules and risk flags, when the project could be read.
    pub summary: Option<(usize, usize)>,
    /// Why it could not be read, already safe to show.
    pub error: Option<String>,
}

/// What was found in the file.
#[derive(Debug, Default)]
pub struct Opened {
    /// The VBA project, if the file has one.
    pub part: Option<VbaPart>,
    /// Where it was found, such as `xl/vbaProject.bin` or
    /// `_VBA_PROJECT_CUR`. Raw: show it with [`text::name`].
    pub location: Option<String>,
    /// A sentence for the person or agent, such as why there are no macros.
    pub note: Option<String>,
    /// Signs that the package hides or swaps a VBA project. Safe to show.
    pub warnings: Vec<String>,
    /// Every VBA project part of a package, the loaded one first; empty
    /// for compound files.
    pub candidates: Vec<Candidate>,
    /// Candidates beyond [`MAX_CANDIDATES`].
    pub more_candidates: usize,
}

impl Opened {
    fn none(note: impl Into<String>) -> Opened {
        Opened {
            note: Some(note.into()),
            ..Opened::default()
        }
    }
}

/// The lowercase extension, such as `xlsm`; empty when there is none.
pub fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Opens a workbook and finds its VBA project.
pub fn open(path: &Path) -> Result<Opened, CommandError> {
    let shown = display_path(path);
    let ext = extension(path);
    let meta = std::fs::metadata(path).map_err(|e| io_error(&shown, e))?;
    if !meta.is_file() {
        return Err(CommandError::invalid_argument(
            "path",
            format!("{shown} is a folder or device, not a workbook file"),
            "Give the path of a workbook file, like book.xlsm.",
        ));
    }
    let mut file = File::open(path).map_err(|e| io_error(&shown, e))?;
    let mut head = [0u8; 8];
    let got = read_up_to(&mut file, &mut head).map_err(|e| io_error(&shown, e))?;
    file.rewind().map_err(|e| io_error(&shown, e))?;
    let head = &head[..got];

    if head == OLE2_MAGIC {
        return open_compound(file, &shown, &ext);
    }
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        return open_package(file, &shown, &ext);
    }
    if matches!(ext.as_str(), "csv" | "tsv" | "txt") {
        return Ok(Opened::none(
            "CSV and text files are plain text and cannot hold macros.",
        ));
    }
    Err(CommandError::new(
        ErrorCode::UnsupportedFormat,
        format!("{shown} is not a workbook Carrysheet can read macros from"),
        "macros live in zip packages (.xlsm, .xlsb, .xltm, .xlam) and compound files (.xls); this file starts like neither",
        "Check that it is an Excel workbook and not cut short. If it should be one, ask the sender for a fresh copy.",
    ))
}

/// Reads into `buf` until it is full or the file ends.
fn read_up_to(file: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn io_error(shown: &str, e: std::io::Error) -> CommandError {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::FileNotFound,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        _ => ErrorCode::Io,
    };
    CommandError::new(
        code,
        format!("cannot read {shown}"),
        text::quoted(&e.to_string(), 200),
        "Check that the file exists, is not open in another program that locks it, and that you can read it.",
    )
}

/// Restoring is the fix for a damaged or hostile file; Excel's repair
/// would run, or keep, whatever the file holds.
const RESTORE: &str = "Restore it from a backup or ask the sender for a fresh copy; do not open it in Excel to repair it.";

fn limit_error(shown: &str, what: &str, max: u64) -> CommandError {
    CommandError::new(
        ErrorCode::LimitExceeded,
        format!("{shown} was refused: {what} is over the limit of {max}"),
        "limits stop damaged or hostile files (such as zip bombs) from using all memory or time; real workbooks stay far below them",
        "Treat the file as hostile and do not open it in Excel. Ask the sender for a smaller copy or the modules exported as text.",
    )
}

fn damaged(shown: &str, what: impl Into<String>, why: impl Into<String>) -> CommandError {
    CommandError::new(
        ErrorCode::DamagedFile,
        format!("{shown}: {}", what.into()),
        why,
        RESTORE,
    )
}

/// Turns an error of the VBA reader into a command error. Its texts can
/// hold module names, so they are quoted.
pub fn vba_error(shown: &str, e: &VbaError) -> CommandError {
    match e {
        VbaError::LimitExceeded { limit, max } => limit_error(shown, limit, *max),
        VbaError::NotCompoundFile { .. } => damaged(
            shown,
            "the VBA project part is not a compound file",
            text::quoted(&format!("{} {}", e.what(), e.why()), 300),
        ),
        _ => damaged(
            shown,
            text::quoted(&e.what(), 200),
            text::quoted(&e.why(), 300),
        ),
    }
}

// ---- .xls (and encrypted packages, which are compound files too) ----

fn open_compound(file: File, shown: &str, ext: &str) -> Result<Opened, CommandError> {
    let mut bytes = Vec::new();
    file.take(MAX_XLS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(shown, e))?;
    if bytes.len() as u64 > MAX_XLS_BYTES {
        return Err(limit_error(shown, "the file size in bytes", MAX_XLS_BYTES));
    }
    match compound_kind(&bytes) {
        CompoundKind::EncryptedPackage => {
            return Err(CommandError::new(
                ErrorCode::UnsupportedFormat,
                format!("{shown} is password-protected (encrypted)"),
                "an encrypted .xlsx, .xlsm or .xlsb is stored as a compound file whose contents, macros included, cannot be read without the password",
                "Open it in Excel with the password, clear it (File > Info > Protect Workbook > Encrypt with Password), save, and try again.",
            ));
        }
        CompoundKind::VbaProjectBin => {
            return Ok(Opened {
                part: Some(VbaPart::from_bin(bytes)),
                location: Some("the whole file (a vbaProject.bin)".into()),
                ..Opened::default()
            });
        }
        CompoundKind::Other => {}
    }
    let limits = Limits::default();
    match VbaPart::from_xls(&bytes, &limits) {
        Ok(Some(part)) => Ok(Opened {
            part: Some(part),
            location: Some("_VBA_PROJECT_CUR".into()),
            ..Opened::default()
        }),
        Ok(None) => Ok(Opened::none(format!(
            "This {} file has no VBA project.",
            if ext.is_empty() {
                "compound".to_owned()
            } else {
                text::name(ext)
            }
        ))),
        Err(e) => Err(vba_error(shown, &e)),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CompoundKind {
    /// An encrypted OOXML package (MS-OFFCRYPTO's `EncryptedPackage`).
    EncryptedPackage,
    /// A `vbaProject.bin` on its own: a `VBA` storage at the root.
    VbaProjectBin,
    /// Anything else, such as an `.xls` workbook.
    Other,
}

/// What a compound file holds, by its root entries. A file the reader
/// cannot open (or panics on) is `Other`, and `from_xls` reports why.
fn compound_kind(bytes: &[u8]) -> CompoundKind {
    catch_unwind(AssertUnwindSafe(|| {
        let Ok(f) = carrysheet_vba::cfb::CompoundFile::open(bytes) else {
            return CompoundKind::Other;
        };
        if f.is_stream("/EncryptedPackage") {
            CompoundKind::EncryptedPackage
        } else if f.is_storage("/VBA") {
            CompoundKind::VbaProjectBin
        } else {
            CompoundKind::Other
        }
    }))
    .unwrap_or(CompoundKind::Other)
}

// ---- zip packages ----

type Package = Archive<BufReader<File>>;

/// The zip reader's limits for a workbook package.
fn zip_limits() -> ZipLimits {
    ZipLimits {
        max_entries: MAX_ZIP_ENTRIES,
        max_total_read: MAX_READ_BYTES,
        // Reported as a warning: the decoy and the real part are both
        // listed, and the first is the one Excel reads.
        duplicate_names: DuplicateNames::Report,
        ..ZipLimits::default()
    }
}

/// Turns a zip reader error into a command error. `entry` names the entry
/// being read, if any; the reader's own texts hold no file text.
fn zip_error(shown: &str, e: ZipError, entry: Option<&str>) -> CommandError {
    let at = entry
        .map(|n| format!(" (in {})", text::name(n)))
        .unwrap_or_default();
    match e {
        ZipError::Io(e) => io_error(shown, e),
        ZipError::LimitExceeded { what, max } => limit_error(shown, &format!("{what}{at}"), max),
        ZipError::Interrupted => CommandError::new(
            ErrorCode::LimitExceeded,
            format!("{shown} was refused: reading it took over {SCAN_SECONDS} seconds"),
            "the package is built to make reading slow (such as deflate data that costs time without producing output); real workbooks take well under a second",
            "Treat the file as hostile and do not open it in Excel. Ask the sender for a fresh copy or the modules exported as text.",
        ),
        ZipError::Unsupported(what) => damaged(
            shown,
            format!("the zip package cannot be read: {what}{at}"),
            "Excel writes only unencrypted Stored and Deflate entries on one disk, so the package was made by another tool or tampered with",
        ),
        e => damaged(
            shown,
            format!("{e}{at}"),
            "the zip package's records disagree, overlap or are cut short, or its compressed data is built to waste time; such a file may show one program different contents than another",
        ),
    }
}

fn open_package(file: File, shown: &str, ext: &str) -> Result<Opened, CommandError> {
    let deadline = Instant::now() + Duration::from_secs(SCAN_SECONDS);
    let mut zip =
        Archive::open(BufReader::new(file), zip_limits()).map_err(|e| zip_error(shown, e, None))?;
    zip.set_interrupt(Box::new(move || Instant::now() > deadline));
    let mut opened = Opened::default();
    let names: Vec<String> = zip.entries().iter().map(|e| e.name.clone()).collect();

    // Two entries with one name: readers disagree on which one counts.
    if !zip.duplicates().is_empty() || !zip.case_twins().is_empty() {
        opened.warnings.push(
            "The zip has entries with the same name (or names that differ only in case); programs may read different parts.".into(),
        );
    }

    let types = content_types(&mut zip, shown)?;
    let (related, missing) = vba_relationships(&mut zip, shown, &mut opened.warnings)?;

    // Every candidate, by entry index, with how it was found.
    let mut found = Found::default();
    for &index in &related {
        found.add(index, FoundBy::Relationship);
    }
    for (index, name) in names.iter().enumerate() {
        if types.is_vba(name) {
            found.add(index, FoundBy::ContentType);
        }
    }
    // Every compound file in the package is opened to see whether it is
    // a VBA project. Each entry is inflated once, and at most two are in
    // memory at a time: the loaded project's bytes are kept for below,
    // every other listed candidate is summed up at once and its bytes
    // dropped. (Keeping the bytes of all eight listed candidates held up
    // to MAX_CANDIDATE_BYTES at once.) A candidate's place in `found`
    // never changes once it is added, so the first is known when its
    // bytes are read.
    let limits = Limits::default();
    let mut budget = MAX_CANDIDATE_BYTES;
    let mut first: Option<(usize, Vec<u8>)> = None;
    let mut summed: HashMap<usize, Summary> = HashMap::new();
    let mut skipped = 0;
    for (index, name) in names.iter().enumerate() {
        if Instant::now() > deadline {
            return Err(zip_error(shown, ZipError::Interrupted, None));
        }
        if !starts_with_ole2(&mut zip, index, name, shown)? {
            continue;
        }
        let size = zip.entries()[index].size;
        if size > MAX_VBA_PART_BYTES && !found.has(index) {
            // Too big for a VBA project, and nothing names it as one.
            skipped += 1;
            continue;
        }
        let bytes = read_entry(&mut zip, index, name, MAX_VBA_PART_BYTES, shown)?;
        budget = budget.checked_sub(bytes.len() as u64).ok_or_else(|| {
            limit_error(
                shown,
                "the bytes of compound-file entries",
                MAX_CANDIDATE_BYTES,
            )
        })?;
        if compound_kind(&bytes) == CompoundKind::VbaProjectBin {
            found.add(index, FoundBy::Contents);
        }
        // Candidates are listed in the order they were found.
        match found.position(index) {
            Some(0) => first = Some((index, bytes)),
            Some(p) if p < MAX_CANDIDATES => {
                summed.insert(index, sum_up(bytes, &limits));
            }
            _ => {}
        }
    }
    if skipped > 0 {
        opened.warnings.push(format!(
            "{skipped} compound file{} over 64 MB in the package {} not checked for macros; a VBA project is never that big, but the file is unusual.",
            if skipped == 1 { "" } else { "s" },
            if skipped == 1 { "was" } else { "were" },
        ));
    }
    let mut found = found.list;

    if found.is_empty() {
        if let Some(target) = missing.first() {
            return Err(damaged(
                shown,
                format!(
                    "the package lists a VBA project at {}, but it is missing",
                    text::name(target)
                ),
                "the workbook's relationships point at a part the zip does not have, so the file was cut short or damaged",
            ));
        }
        let note = match ext {
            "xlsx" | "xltx" => {
                "An .xlsx file cannot hold macros; macro-enabled workbooks are .xlsm, .xlsb or .xls."
            }
            _ => "This workbook has no VBA project.",
        };
        let mut none = Opened::none(note);
        none.warnings = opened.warnings;
        return Ok(none);
    }

    // The loaded project: the relationship's (Excel's choice), else the
    // first by content type, else the first by contents. They were added
    // in that order, so this keeps the order the bytes were kept by.
    found.sort_by_key(|(_, by)| {
        if by.contains(&FoundBy::Relationship) {
            0
        } else if by.contains(&FoundBy::ContentType) {
            1
        } else {
            2
        }
    });
    if found.len() > 1 {
        opened.warnings.push(format!(
            "The package holds {} VBA projects; Excel loads only the first listed in vba_parts. The others may be decoys or hidden code.",
            found.len()
        ));
    }
    if !missing.is_empty() {
        opened.warnings.push(
            "The workbook's relationships point at a VBA project part the zip does not have."
                .into(),
        );
    }
    if related.is_empty() {
        opened.warnings.push(
            "No workbook relationship names this VBA project, so Excel may not load it; the code is still in the file.".into(),
        );
    }
    let mut primary = None;
    opened.more_candidates = found.len().saturating_sub(MAX_CANDIDATES);
    // Only the listed candidates are read, each at most once; those that
    // are not compound files were not read above, and are read here one
    // at a time.
    for (n, (index, by)) in found.iter().take(MAX_CANDIDATES).enumerate() {
        if n == 0 {
            primary = Some(match first.take() {
                Some((at, bytes)) if at == *index => bytes,
                _ => read_entry(&mut zip, *index, &names[*index], MAX_VBA_PART_BYTES, shown)?,
            });
            continue;
        }
        let (summary, error) = match summed.remove(index) {
            Some(done) => done,
            None => sum_up(
                read_entry(&mut zip, *index, &names[*index], MAX_VBA_PART_BYTES, shown)?,
                &limits,
            ),
        };
        opened.candidates.push(Candidate {
            name: names[*index].clone(),
            found_by: by.clone(),
            summary,
            error,
        });
    }
    let (index, by) = &found[0];
    opened.candidates.insert(
        0,
        Candidate {
            name: names[*index].clone(),
            found_by: by.clone(),
            summary: None,
            error: None,
        },
    );
    opened.location = Some(names[*index].clone());
    opened.part = primary.map(VbaPart::from_bin);
    if matches!(ext, "xlsx" | "xltx") {
        opened.note = Some(
            "This .xlsx holds a VBA project, which .xlsx files should not; it may be a renamed .xlsm. Excel will refuse to open it as is.".into(),
        );
    }
    Ok(opened)
}

/// A listed candidate other than the loaded project: its modules and risk
/// flags when it reads, else why not (safe to show).
type Summary = (Option<(usize, usize)>, Option<String>);

fn sum_up(bytes: Vec<u8>, limits: &Limits) -> Summary {
    let read = VbaPart::from_bin(bytes).read(limits);
    (
        read.as_ref().ok().map(|p| (p.modules.len(), p.flags.len())),
        read.err().map(|e| text::quoted(&e.what(), 200)),
    )
}

/// Candidates in the order found, each with how it was found.
#[derive(Default)]
struct Found {
    list: Vec<(usize, Vec<FoundBy>)>,
    at: HashMap<usize, usize>,
}

impl Found {
    fn add(&mut self, index: usize, by: FoundBy) {
        match self.at.get(&index) {
            Some(&pos) => {
                let list = &mut self.list[pos].1;
                if !list.contains(&by) {
                    list.push(by);
                }
            }
            None => {
                self.at.insert(index, self.list.len());
                self.list.push((index, vec![by]));
            }
        }
    }

    fn has(&self, index: usize) -> bool {
        self.at.contains_key(&index)
    }

    fn position(&self, index: usize) -> Option<usize> {
        self.at.get(&index).copied()
    }
}

/// Content types by part: `Override` by name, else `Default` by extension.
#[derive(Debug, Default)]
struct ContentTypes {
    defaults: HashMap<String, String>,
    overrides: HashMap<String, String>,
}

impl ContentTypes {
    fn is_vba(&self, entry: &str) -> bool {
        let key = entry.to_lowercase();
        let ty = self.overrides.get(&key).or_else(|| {
            let ext = key.rsplit_once('.').map(|(_, e)| e)?;
            (!ext.contains('/')).then(|| self.defaults.get(ext))?
        });
        ty.is_some_and(|t| t.trim().eq_ignore_ascii_case(VBA_CONTENT_TYPE))
    }
}

fn content_types(zip: &mut Package, shown: &str) -> Result<ContentTypes, CommandError> {
    let mut types = ContentTypes::default();
    let Some(xml_text) = read_xml(zip, "[Content_Types].xml", shown)? else {
        return Ok(types);
    };
    let elements = scan(
        &xml_text,
        CONTENT_TYPES_NAMESPACE,
        &["Default", "Override"],
        "[Content_Types].xml",
        shown,
    )?;
    for e in elements {
        let Some(ty) = e.attr("ContentType") else {
            continue;
        };
        if e.name == "Default" {
            if let Some(ext) = e.attr("Extension") {
                types.defaults.insert(ext.to_lowercase(), ty);
            }
        } else if let Some(part) = e.attr("PartName") {
            types
                .overrides
                .insert(resolve("", &percent_decode(&part)).to_lowercase(), ty);
        }
    }
    Ok(types)
}

/// The entries the workbook's VBA relationships point at, in order and
/// without repeats, and the targets that are not in the zip.
fn vba_relationships(
    zip: &mut Package,
    shown: &str,
    warnings: &mut Vec<String>,
) -> Result<(Vec<usize>, Vec<String>), CommandError> {
    let mut related = Vec::new();
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    let Some(root) = read_xml(zip, "_rels/.rels", shown)? else {
        return Ok((related, missing));
    };
    let mut workbooks: Vec<String> = Vec::new();
    let mut distinct = HashSet::new();
    for e in scan(
        &root,
        RELATIONSHIPS_NAMESPACE,
        &["Relationship"],
        "_rels/.rels",
        shown,
    )? {
        if !is_internal(&e) || !is_type(&e, OFFICE_DOCUMENT_RELATIONSHIPS) {
            continue;
        }
        let Some(target) = e.attr("Target") else {
            continue;
        };
        let workbook = resolve("", &percent_decode(&target));
        if distinct.insert(workbook.to_ascii_lowercase()) {
            workbooks.push(workbook);
        }
    }
    if workbooks.len() > 1 {
        warnings.push(
            "The package names more than one main workbook part; programs may open different ones."
                .into(),
        );
    }
    for workbook in workbooks.iter().take(MAX_CANDIDATES) {
        let (dir, file) = match workbook.rfind('/') {
            Some(i) => (&workbook[..=i], &workbook[i + 1..]),
            None => ("", workbook.as_str()),
        };
        let rels_name = format!("{dir}_rels/{file}.rels");
        let Some(rels) = read_xml(zip, &rels_name, shown)? else {
            continue;
        };
        for e in scan(
            &rels,
            RELATIONSHIPS_NAMESPACE,
            &["Relationship"],
            &rels_name,
            shown,
        )? {
            if !is_internal(&e) || !is_type(&e, &[VBA_RELATIONSHIP]) {
                continue;
            }
            let Some(target) = e.attr("Target") else {
                continue;
            };
            let target = resolve(dir, &percent_decode(&target));
            match zip.find_ignore_case(&target) {
                Some(i) => {
                    if seen.insert(i) {
                        related.push(i);
                    }
                }
                None if missing.len() < MAX_CANDIDATES => missing.push(target),
                None => {}
            }
        }
    }
    Ok((related, missing))
}

fn scan<'a>(
    xml_text: &'a str,
    namespace: &str,
    wanted: &[&str],
    part: &str,
    shown: &str,
) -> Result<Vec<xml::Element<'a>>, CommandError> {
    xml::elements(xml_text, namespace, wanted, MAX_XML_ELEMENTS).map_err(|e| match e {
        XmlError::Doctype => damaged(
            shown,
            format!("{} has a DOCTYPE", text::name(part)),
            "package parts must not declare a document type (Open Packaging Conventions); Office refuses such files",
        ),
        XmlError::TooMany => limit_error(
            shown,
            &format!("the number of elements in {}", text::name(part)),
            MAX_XML_ELEMENTS as u64,
        ),
    })
}

fn is_internal(e: &xml::Element) -> bool {
    e.attr("TargetMode")
        .is_none_or(|m| !m.trim().eq_ignore_ascii_case("External"))
}

fn is_type(e: &xml::Element, types: &[&str]) -> bool {
    e.attr("Type").is_some_and(|t| types.contains(&t.trim()))
}

/// True when an entry's data starts like a compound file. Encrypted
/// entries and other compression methods are skipped here (Excel cannot
/// read them either); if they are candidates by name, reading them
/// reports why. Any other error refuses the file: "no macros" is said
/// only after every entry was checked.
fn starts_with_ole2(
    zip: &mut Package,
    index: usize,
    name: &str,
    shown: &str,
) -> Result<bool, CommandError> {
    let entry = &zip.entries()[index];
    if entry.is_dir() || entry.size < 8 {
        return Ok(false);
    }
    match zip.read_prefix(index, 8) {
        Ok(head) => Ok(head == OLE2_MAGIC),
        Err(ZipError::Unsupported(_)) => Ok(false),
        Err(e) => Err(zip_error(shown, e, Some(name))),
    }
}

/// Reads one entry, refusing encryption, unusual compression and anything
/// over `max` bytes, whatever its header claims.
fn read_entry(
    zip: &mut Package,
    index: usize,
    name: &str,
    max: u64,
    shown: &str,
) -> Result<Vec<u8>, CommandError> {
    zip.read(index, max)
        .map_err(|e| zip_error(shown, e, Some(name)))
}

/// Reads a small XML part as text, if the package has it.
fn read_xml(zip: &mut Package, name: &str, shown: &str) -> Result<Option<String>, CommandError> {
    let Some(index) = zip.find_ignore_case(name) else {
        return Ok(None);
    };
    let bytes = read_entry(zip, index, name, MAX_XML_PART_BYTES, shown)?;
    Ok(Some(xml_text(&bytes)))
}

/// Decodes an XML part: UTF-8, or UTF-16 with a byte order mark or, as
/// XML allows, without one when it starts with `<` (`<\0` or `\0<`).
fn xml_text(bytes: &[u8]) -> String {
    let utf16 = |body: &[u8], be: bool| {
        let units: Vec<u16> = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| {
                if be {
                    u16::from_be_bytes(c)
                } else {
                    u16::from_le_bytes(c)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        [b'<', 0, ..] => utf16(bytes, false),
        [0, b'<', ..] => utf16(bytes, true),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Decodes `%XX` escapes in a part name or target (as UTF-8).
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolves a relationship target against the folder of its source part
/// (`dir` ends with `/` or is empty), as a zip entry name. `..` stops at
/// the package root, as a URI path does.
fn resolve(dir: &str, target: &str) -> String {
    let target = target.trim();
    let joined = match target.strip_prefix('/') {
        Some(absolute) => absolute.to_owned(),
        None => format!("{dir}{target}"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_resolve_inside_the_package() {
        assert_eq!(resolve("xl/", "vbaProject.bin"), "xl/vbaProject.bin");
        assert_eq!(resolve("xl/", "/xl/vbaProject.bin"), "xl/vbaProject.bin");
        assert_eq!(resolve("xl/", "../a/./b.bin"), "a/b.bin");
        // `..` stops at the root, as in a URI.
        assert_eq!(resolve("xl/", "../../../xl/code.bin"), "xl/code.bin");
        assert_eq!(resolve("", ""), "");
    }

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(percent_decode("vba%50roject.bin"), "vbaProject.bin");
        assert_eq!(percent_decode("a%20b%"), "a b%");
        assert_eq!(percent_decode("%C3%B3"), "ó");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn content_types_use_override_then_default() {
        let mut t = ContentTypes::default();
        t.defaults.insert("bin".into(), VBA_CONTENT_TYPE.into());
        t.overrides.insert(
            "xl/printersettings/p1.bin".into(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings".into(),
        );
        assert!(t.is_vba("xl/Code.BIN"));
        assert!(!t.is_vba("xl/printerSettings/p1.bin"));
        assert!(!t.is_vba("xl/workbook.xml"));
        assert!(!t.is_vba("noext"));
    }

    #[test]
    fn xml_text_handles_byte_order_marks() {
        assert_eq!(xml_text(b"\xEF\xBB\xBF<a/>"), "<a/>");
        assert_eq!(xml_text(b"\xFF\xFE<\0a\0"), "<a");
        assert_eq!(xml_text(b"\xFE\xFF\0<\0a"), "<a");
        // UTF-16 without a byte order mark, told by the first `<`.
        assert_eq!(xml_text(b"<\0a\0/\0>\0"), "<a/>");
        assert_eq!(xml_text(b"\0<\0a\0/\0>"), "<a/>");
    }
}
