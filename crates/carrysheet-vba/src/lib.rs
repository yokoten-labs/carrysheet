//! Read VBA macro projects without running them, and keep them byte for byte.
//!
//! Excel stores macros as a VBA project (Microsoft's public MS-OVBA
//! format): `xl/vbaProject.bin` inside `.xlsm` and `.xlsb` packages, or the
//! `_VBA_PROJECT_CUR` storage inside `.xls` files. This crate lists the
//! modules, decompresses their source, and flags code that may be risky.
//! It never runs, edits or re-encodes macros.
//!
//! ```no_run
//! use carrysheet_vba::{Limits, VbaPart};
//!
//! # fn demo(vba_project_bin: Vec<u8>) -> Result<(), carrysheet_vba::VbaError> {
//! // Bytes of xl/vbaProject.bin, taken from the .xlsm package by the caller.
//! let part = VbaPart::from_bin(vba_project_bin);
//! let project = part.read(&Limits::default())?;
//! for module in &project.modules {
//!     println!("{} ({:?}, {} lines)", module.name, module.kind, module.line_count);
//! }
//! for flag in &project.flags {
//!     println!("{}: {} line {:?}: {}", flag.kind.as_str(), flag.module, flag.line, flag.why);
//! }
//! // Saving: write the part back exactly as it came.
//! let unchanged: &[u8] = part.bin_bytes().unwrap();
//! # let _ = unchanged;
//! # Ok(())
//! # }
//! ```
//!
//! # Keeping macros on save
//!
//! [`VbaPart`] holds the project exactly as found. For `vbaProject.bin`
//! that is the original bytes; for `.xls` it is every stream of the VBA
//! storage with its metadata ([`KeptStorage`]), which
//! [`KeptStorage::write_into`] writes into the new compound file. The
//! digital signature streams of `.xls` files live next to the storage, in
//! the workbook's root, and are the caller's to keep.
//!
//! # Hostile input
//!
//! Every size and count is bounded by [`Limits`]. The compound file reader
//! (carrysheet-cfb) returns an error on a damaged container, which becomes
//! a [`VbaError::Container`]; it never panics. A module that cannot be
//! read does not stop the others: its [`Module::error`] says why.

mod decompress;
mod dir;
mod error;
mod project_stream;
mod risk;
mod storage;

/// The compound file reader and writer, for [`VbaPart::from_storage`] and
/// [`KeptStorage::write_into`].
pub use carrysheet_cfb as cfb;
pub use decompress::decompress;
pub use error::VbaError;
pub use risk::RiskKind;
pub use storage::{KeptEntry, KeptStorage, VbaPart, XLS_VBA_STORAGE};

use std::collections::HashMap;
use std::rc::Rc;

use encoding_rs::Encoding;

/// Safety limits for reading. The defaults are far above real projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of the input (`vbaProject.bin` or `.xls` file).
    pub max_input_bytes: usize,
    /// Bytes of any one stream in the compound file.
    pub max_stream_bytes: usize,
    /// Bytes of all streams in the VBA storage together.
    pub max_storage_bytes: usize,
    /// Storages and streams in the VBA storage.
    pub max_entries: usize,
    /// Bytes one compressed container may decompress to.
    pub max_decompressed_bytes: usize,
    /// Modules in one project.
    pub max_modules: usize,
    /// References in one project.
    pub max_references: usize,
    /// Risk flags reported for one project. When findings are left out
    /// for it, one more flag, "Flag list cut short", is added at the end.
    pub max_flags: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_input_bytes: 512 << 20,
            max_stream_bytes: 64 << 20,
            max_storage_bytes: 256 << 20,
            max_entries: 20_000,
            max_decompressed_bytes: 16 << 20,
            max_modules: 4_096,
            max_references: 4_096,
            max_flags: 10_000,
        }
    }
}

/// A VBA project as read from the file.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VbaProject {
    /// The project name, such as `VBAProject`.
    pub name: String,
    /// The Windows code page of the project's text (PROJECTCODEPAGE).
    pub code_page: u16,
    /// The encoding used to decode text, such as `windows-1252`.
    pub encoding: &'static str,
    /// False when the code page is unknown and `windows-1252` was assumed.
    pub code_page_known: bool,
    /// The VBA project version (major, minor), when present.
    pub version: Option<(u32, u16)>,
    /// Protection as the `PROJECT` stream reports it.
    pub protection: Protection,
    /// Libraries the project uses.
    pub references: Vec<Reference>,
    /// Modules in `dir` stream order.
    pub modules: Vec<Module>,
    /// Risk flags, by module and line.
    pub flags: Vec<RiskFlag>,
    /// Whether the `PROJECT` stream was there. Without it, module kinds
    /// are guessed and hidden modules cannot be found.
    pub has_project_stream: bool,
}

/// Password and visibility state, as recorded in the `PROJECT` stream
/// (MS-OVBA 2.3.1.15 to 2.3.1.17). This reports what the file says; it
/// never reveals a password.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Protection {
    /// A `CMG=` (protection state) line is present.
    pub cmg_present: bool,
    /// A `DPB=` (password) line is present.
    pub dpb_present: bool,
    /// A `GC=` (visibility) line is present.
    pub gc_present: bool,
    /// The user protected the project (from `CMG`).
    pub user_protected: Option<bool>,
    /// The host application protected the project (from `CMG`).
    pub host_protected: Option<bool>,
    /// The VBA editor protected the project (from `CMG`).
    pub vbe_protected: Option<bool>,
    /// What `DPB` holds.
    pub password: PasswordState,
    /// Whether the project is visible in the editor (from `GC`).
    pub visible: Option<bool>,
}

impl Protection {
    /// True when Excel asks for a password before showing the code.
    /// The source is still readable by this crate.
    pub fn locked_for_viewing(&self) -> bool {
        self.vbe_protected == Some(true) || self.visible == Some(false)
    }
}

/// What the `DPB` line holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum PasswordState {
    /// No `DPB` line was read.
    #[default]
    Unknown,
    /// No password.
    None,
    /// A salted password hash.
    Hash,
    /// The password itself, lightly obfuscated. Its value is never kept.
    PlainText,
    /// The value could not be decoded.
    Unreadable,
}

/// A library the project references.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reference {
    /// The name, such as `stdole` or `Office`.
    pub name: Option<String>,
    /// How it is referenced.
    pub kind: ReferenceKind,
    /// The library identifier, such as
    /// `*\G{00020430-0000-0000-C000-000000000046}#2.0#0#C:\Windows\System32\stdole2.tlb#OLE Automation`.
    pub libid: String,
}

/// How a library is referenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReferenceKind {
    /// A registered type library (REFERENCEREGISTERED).
    Registered,
    /// Another VBA project (REFERENCEPROJECT).
    Project,
    /// An ActiveX control library (REFERENCECONTROL).
    Control,
}

/// What a module is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModuleKind {
    /// A standard module (`Module1`).
    Standard,
    /// A class module.
    Class,
    /// The code behind the workbook or a sheet (`ThisWorkbook`, `Sheet1`).
    Document,
    /// A UserForm (designer module).
    Form,
}

/// One module of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Module {
    /// The module name.
    pub name: String,
    /// The name of its stream in the `VBA` storage.
    pub stream_name: String,
    /// What the module is.
    pub kind: ModuleKind,
    /// The source text, including `Attribute` lines, with the line breaks
    /// as stored (CR LF; CR alone from Mac Excel). `None` when there is no
    /// source.
    pub source: Option<String>,
    /// Lines in `source`, split at CR LF, LF or CR.
    pub line_count: usize,
    /// True when the source has at least one line that is not blank and
    /// not an `Attribute` line. False means p-code only, an empty module,
    /// or source that could not be read.
    pub has_source: bool,
    /// Bytes of compiled code (p-code) before the source in the stream
    /// (MODULEOFFSET), known even when the source cannot be read.
    pub pcode_bytes: usize,
    /// Bytes of compressed source in the stream.
    pub compressed_source_bytes: usize,
    /// In the `dir` stream but not listed in the `PROJECT` stream.
    pub hidden: bool,
    /// MODULEREADONLY is set.
    pub read_only: bool,
    /// MODULEPRIVATE is set.
    pub private: bool,
    /// Why this module could not be read, if it could not.
    pub error: Option<VbaError>,
}

impl Module {
    #[cfg(test)]
    pub(crate) fn test_module(name: &str, source: &str) -> Module {
        Module {
            name: name.into(),
            stream_name: name.into(),
            kind: ModuleKind::Standard,
            source: Some(source.into()),
            line_count: source_lines(source).count(),
            has_source: true,
            pcode_bytes: 0,
            compressed_source_bytes: 0,
            hidden: false,
            read_only: false,
            private: false,
            error: None,
        }
    }
}

/// A finding that a person should look at. Reading never runs code.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RiskFlag {
    /// The kind of risk.
    pub kind: RiskKind,
    /// The rule that matched, such as `Workbook_Open` or `Shell`.
    pub rule: &'static str,
    /// The module name for display only: control, invisible and
    /// bidirectional characters escaped, backslashes doubled, cut at about
    /// 200 characters. Two modules can show the same text; do not use it to
    /// find a module.
    pub module: String,
    /// The module's index in [`VbaProject::modules`]: the key to the module.
    pub module_index: usize,
    /// The line in [`Module::source`], counted from 1; `None` for
    /// module-level findings. For a statement continued with ` _`, the
    /// first of its lines.
    pub line: Option<u32>,
    /// Why it matters, in one short sentence.
    pub why: &'static str,
}

/// Reads the macros of a `vbaProject.bin` part with default limits.
pub fn read_vba_project_bin(bytes: &[u8]) -> Result<VbaProject, VbaError> {
    let limits = Limits::default();
    let file = storage::open_compound(bytes, &limits)?;
    VbaPart::from_storage(&file, "/", &limits)?.read(&limits)
}

/// Reads the macros of a whole `.xls` file with default limits. Returns
/// `Ok(None)` when the workbook has no VBA project.
pub fn read_xls_vba(xls: &[u8]) -> Result<Option<VbaProject>, VbaError> {
    let limits = Limits::default();
    match VbaPart::from_xls(xls, &limits)? {
        Some(part) => part.read(&limits).map(Some),
        None => Ok(None),
    }
}

/// The encoding for a Windows code page. Unknown pages fall back to
/// Windows-1252 and return `false`.
pub fn encoding_for_code_page(code_page: u16) -> (&'static Encoding, bool) {
    use encoding_rs::*;
    let e = match code_page {
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        866 => IBM866,
        1200 => UTF_16LE,
        1201 => UTF_16BE,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        // 20127 (US-ASCII) and 28591 (Latin-1) read as their superset.
        1252 | 20127 | 28591 => WINDOWS_1252,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        10000 => MACINTOSH,
        10007 => X_MAC_CYRILLIC,
        20866 => KOI8_R,
        21866 => KOI8_U,
        20932 | 51932 => EUC_JP,
        28592 => ISO_8859_2,
        28593 => ISO_8859_3,
        28594 => ISO_8859_4,
        28595 => ISO_8859_5,
        28596 => ISO_8859_6,
        28597 => ISO_8859_7,
        28598 => ISO_8859_8,
        28603 => ISO_8859_13,
        28605 => ISO_8859_15,
        50220..=50222 => ISO_2022_JP,
        54936 => GB18030,
        65001 => UTF_8,
        _ => return (WINDOWS_1252, false),
    };
    (e, true)
}

/// Splits source text into lines at CR LF, LF or a lone CR (Mac Excel
/// writes CR only). A final line break does not start another line.
pub(crate) fn source_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(text).filter(|t| !t.is_empty());
    std::iter::from_fn(move || {
        let t = rest?;
        match t.find(['\r', '\n']) {
            Some(i) => {
                let skip = if t[i..].starts_with("\r\n") { 2 } else { 1 };
                rest = Some(&t[i + skip..]).filter(|r| !r.is_empty());
                Some(&t[..i])
            }
            None => {
                rest = None;
                Some(t)
            }
        }
    })
}

fn decode(bytes: &[u8], encoding: &'static Encoding) -> String {
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

fn decode_utf16le(bytes: &[u8]) -> String {
    encoding_rs::UTF_16LE
        .decode_without_bom_handling(bytes)
        .0
        .into_owned()
}

/// Reads a project from its kept storage.
pub(crate) fn read_kept(kept: &KeptStorage, limits: &Limits) -> Result<VbaProject, VbaError> {
    let dir_raw = kept
        .stream("VBA/dir")
        .ok_or_else(|| VbaError::MissingStream {
            stream: "VBA/dir".into(),
        })?;
    let dir_bytes =
        decompress(dir_raw, limits.max_decompressed_bytes).map_err(|e| e.in_stream("VBA/dir"))?;
    let dir = dir::parse(&dir_bytes, limits)?;
    let code_page = dir.code_page.unwrap_or(1252);
    let (encoding, code_page_known) = encoding_for_code_page(code_page);

    let project = kept
        .stream("PROJECT")
        .map(|b| project_stream::parse(&decode(b, encoding), limits.max_modules));

    let references = dir
        .references
        .iter()
        .map(|r| Reference {
            name: r
                .name_unicode
                .as_deref()
                .map(decode_utf16le)
                .or_else(|| r.name.as_deref().map(|n| decode(n, encoding))),
            kind: match r.kind {
                dir::RawReferenceKind::Registered => ReferenceKind::Registered,
                dir::RawReferenceKind::Project => ReferenceKind::Project,
                dir::RawReferenceKind::Control => ReferenceKind::Control,
            },
            libid: decode(&r.libid, encoding),
        })
        .collect();

    // Module names the PROJECT stream lists, compared without case.
    let mut listed: HashMap<String, ModuleKind> = HashMap::new();
    if let Some(p) = &project {
        for (n, k) in &p.modules {
            listed.entry(storage::name_key(n)).or_insert(*k);
        }
    }

    let mut modules = Vec::with_capacity(dir.modules.len());
    let mut flags = Vec::new();
    let mut cut_reported = false;
    let mut sources = Sources::default();
    for raw in &dir.modules {
        let mbcs_name = decode(&raw.name, encoding);
        let name = match raw.name_unicode.as_deref() {
            Some(u) if !u.is_empty() => decode_utf16le(u),
            _ => mbcs_name.clone(),
        };
        let stream_name = match raw.stream_name_unicode.as_deref() {
            Some(u) if !u.is_empty() => decode_utf16le(u),
            _ => decode(&raw.stream_name, encoding),
        };
        let listed_kind = listed
            .get(&storage::name_key(&mbcs_name))
            .or_else(|| listed.get(&storage::name_key(&name)))
            .copied();
        let kind = match (listed_kind, raw.procedural) {
            (Some(k), _) => k,
            (None, Some(true)) => ModuleKind::Standard,
            (None, _) => ModuleKind::Class,
        };
        let hidden = project.is_some() && listed_kind.is_none();
        let mut module = Module {
            name,
            stream_name,
            kind,
            source: None,
            line_count: 0,
            has_source: false,
            pcode_bytes: 0,
            compressed_source_bytes: 0,
            hidden,
            read_only: raw.read_only,
            private: raw.private,
            error: None,
        };
        // Once the cut is reported, nothing more can be added: skip scans.
        let scan = (!cut_reported).then(|| ScanRequest {
            room: limits.max_flags.saturating_sub(flags.len()),
            events: kind != ModuleKind::Standard,
        });
        let read = sources.read(kept, raw, &module, encoding, limits, scan)?;
        module.pcode_bytes = read.pcode;
        module.compressed_source_bytes = read.compressed;
        let mut allowance = 0;
        let hits: &[(usize, u32)] = read.scan.as_ref().map_or(&[], |s| &s.hits);
        let info = match read.source {
            Ok(info) => info,
            Err(e) => {
                module.error = Some(e);
                None
            }
        };
        if let Some(info) = &info {
            module.line_count = info.line_count;
            module.has_source = info.text_lines > 0;
            module.source = Some(info.text.clone());
            allowance = info.allowance;
        }
        let facts = risk::ModuleFacts {
            kind: module.kind,
            hidden: module.hidden,
            pcode_bytes: module.pcode_bytes,
            source_present: module.source.is_some(),
            unreadable: module.error.is_some(),
            allowance,
        };
        let dropped = risk::module_flags(
            modules.len(),
            &module,
            &facts,
            hits,
            &mut flags,
            limits.max_flags,
        );
        let scan_cut = read.scan.as_ref().is_some_and(|s| s.cut);
        if !cut_reported && (dropped || scan_cut) {
            flags.push(risk::cut_flag(modules.len(), &module));
            cut_reported = true;
        }
        modules.push(module);
    }

    Ok(VbaProject {
        name: decode(&dir.name, encoding),
        code_page,
        encoding: encoding.name(),
        code_page_known,
        version: dir.version,
        protection: project
            .as_ref()
            .map(|p| p.protection.clone())
            .unwrap_or_default(),
        references,
        modules,
        flags,
        has_project_stream: project.is_some(),
    })
}

/// What reading one module's source found.
struct SourceRead {
    pcode: usize,
    compressed: usize,
    source: Result<Option<Rc<SourceInfo>>, VbaError>,
    /// Line findings, when a scan was asked for and there is source.
    scan: Option<Rc<risk::Scan>>,
}

/// How to scan a module's source.
#[derive(Clone, Copy)]
struct ScanRequest {
    /// Room left in the project's flag list.
    room: usize,
    /// Control and form events can fire in this module.
    events: bool,
}

/// A decoded source, shared by every module that points at the same
/// stream and offset.
struct SourceInfo {
    text: String,
    line_count: usize,
    text_lines: usize,
    allowance: usize,
}

/// Compressed bytes read beyond the storage size before it is a limit.
const COMPRESSED_SLACK: usize = 64 * 1024;

/// Decompressed sources, so modules that share a stream and offset cost
/// one decompression, and the budgets that bound the work.
#[derive(Default)]
struct Sources {
    cache: HashMap<(String, u32), Result<Rc<SourceInfo>, VbaError>>,
    /// Scans by source and whether events count: at most two per source.
    scans: HashMap<(String, u32, bool), Rc<risk::Scan>>,
    /// Bytes produced by decompression, failed attempts included.
    decompressed: usize,
    /// Compressed bytes handed to the decompressor. Modules of a real
    /// project never overlap, so this stays within the storage size;
    /// modules pointing at many offsets of one stream would not.
    compressed_read: usize,
    /// Bytes of source text handed out to modules.
    decoded: usize,
}

impl Sources {
    /// Reads a module's source. Errors that concern only this module are
    /// in `source`; a limit stops the whole project.
    fn read(
        &mut self,
        kept: &KeptStorage,
        raw: &dir::RawModule,
        module: &Module,
        encoding: &'static Encoding,
        limits: &Limits,
        scan: Option<ScanRequest>,
    ) -> Result<SourceRead, VbaError> {
        let path = format!("VBA/{}", module.stream_name);
        let fail = |pcode, e| {
            Ok(SourceRead {
                pcode,
                compressed: 0,
                source: Err(e),
                scan: None,
            })
        };
        let Some(data) = kept.stream(&path) else {
            return fail(0, VbaError::MissingStream { stream: path });
        };
        let Some(offset) = raw.text_offset else {
            // Without an offset the whole stream may be p-code.
            let e = VbaError::dir(
                raw.offset,
                format!(
                    "module `{}` has no MODULEOFFSET record",
                    error::printable(&module.name)
                ),
            );
            return fail(data.len(), e);
        };
        let start = offset as usize;
        if start > data.len() {
            let e = VbaError::BadModuleOffset {
                module: module.name.clone(),
                offset: u64::from(offset),
                stream_len: data.len() as u64,
            };
            return fail(data.len(), e);
        }
        let compressed = &data[start..];
        let mut read = SourceRead {
            pcode: start,
            compressed: compressed.len(),
            source: Ok(None),
            scan: None,
        };
        if compressed.is_empty() {
            return Ok(read);
        }
        let key = (storage::name_key(&path), offset);
        if !self.cache.contains_key(&key) {
            // Charge the input before walking it, so overlapping offsets
            // cannot make the decompressor read the same bytes again and
            // again.
            let budget = kept.total_bytes().saturating_add(COMPRESSED_SLACK);
            self.compressed_read = self.compressed_read.saturating_add(compressed.len());
            if self.compressed_read > budget {
                return Err(VbaError::limit(
                    "compressed source bytes read (more than the VBA storage holds)",
                    budget,
                ));
            }
            let (result, produced) =
                decompress::decompress_counted(compressed, limits.max_decompressed_bytes);
            self.decompressed = self.decompressed.saturating_add(produced);
            if self.decompressed > limits.max_storage_bytes {
                return Err(VbaError::limit(
                    "decompressed source bytes in the project",
                    limits.max_storage_bytes,
                ));
            }
            let entry = match result {
                Ok(bytes) => {
                    let text = decode(&bytes, encoding);
                    // The cache keeps a copy too: count it.
                    self.decoded = self.decoded.saturating_add(text.len());
                    let stats = risk::source_stats(&text);
                    Ok(Rc::new(SourceInfo {
                        line_count: stats.lines,
                        text_lines: stats.text_lines,
                        allowance: stats.allowance,
                        text,
                    }))
                }
                Err(e @ VbaError::LimitExceeded { .. }) => return Err(e),
                Err(e) => Err(e.in_stream(&path)),
            };
            self.cache.insert(key.clone(), entry);
        }
        read.source = match &self.cache[&key] {
            Ok(info) => {
                // Each module gets its own copy of the text: count it.
                self.decoded = self.decoded.saturating_add(info.text.len());
                if self.decoded > limits.max_storage_bytes {
                    return Err(VbaError::limit(
                        "source text in the project",
                        limits.max_storage_bytes,
                    ));
                }
                if let Some(req) = scan {
                    let scan_key = (key.0.clone(), key.1, req.events);
                    let found = self.scans.entry(scan_key).or_insert_with(|| {
                        Rc::new(risk::scan_source(&info.text, req.room, req.events))
                    });
                    read.scan = Some(Rc::clone(found));
                }
                Ok(Some(Rc::clone(info)))
            }
            Err(e) => Err(e.clone()),
        };
        Ok(read)
    }
}
