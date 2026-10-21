//! Walks the workbook stream: the globals substream, then each sheet.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use encoding_rs::Encoding;

use crate::Limits;
use crate::codepage;
use crate::error::{BiffError, BiffErrorKind, Encryption};
use crate::formula::{self, Env, ExternSheet, SupBook, SupBookKind, Xti};
use crate::model::formats;
use crate::model::*;
use crate::stream::bytes::Rd;
use crate::stream::record::{self, CRd, Rec, Records};

// Record types. Where BIFF versions differ the comment says which.
const DIMENSIONS_2: u16 = 0x0000; // BIFF2
const BLANK_2: u16 = 0x0001; // BIFF2
const INTEGER_2: u16 = 0x0002; // BIFF2
const NUMBER_2: u16 = 0x0003; // BIFF2
const LABEL_2: u16 = 0x0004; // BIFF2
const BOOLERR_2: u16 = 0x0005; // BIFF2
const FORMULA: u16 = 0x0006; // BIFF2, BIFF5, BIFF8
const STRING_2: u16 = 0x0007; // BIFF2
const BOF_2: u16 = 0x0009;
const EOF: u16 = 0x000A;
const INDEX_2: u16 = 0x000B;
const EXTERNSHEET: u16 = 0x0017;
const NAME: u16 = 0x0018; // BIFF2, BIFF5, BIFF8
const FORMAT_2: u16 = 0x001E; // BIFF2, BIFF3
const ARRAY_2: u16 = 0x0021; // BIFF2
const DATEMODE: u16 = 0x0022;
const EXTERNNAME: u16 = 0x0023; // BIFF2, BIFF5, BIFF8
const COLWIDTH_2: u16 = 0x0024; // BIFF2
const FILEPASS: u16 = 0x002F;
const CODEPAGE: u16 = 0x0042;
const XF_2: u16 = 0x0043; // BIFF2
const IXFE_2: u16 = 0x0044; // BIFF2
const COLINFO: u16 = 0x007D;
const BOUNDSHEET: u16 = 0x0085;
const BUNDLESHEET: u16 = 0x008F; // BIFF4W
const MULRK: u16 = 0x00BD;
const MULBLANK: u16 = 0x00BE;
const SHRFMLA_OLD: u16 = 0x00BC;
const RSTRING: u16 = 0x00D6;
const DBCELL: u16 = 0x00D7;
const XF: u16 = 0x00E0; // BIFF5, BIFF8
const MERGEDCELLS: u16 = 0x00E5;
const SST: u16 = 0x00FC;
const LABELSST: u16 = 0x00FD;
const EXTSST: u16 = 0x00FF;
const SUPBOOK: u16 = 0x01AE;
const DIMENSIONS: u16 = 0x0200;
const BLANK: u16 = 0x0201;
const NUMBER: u16 = 0x0203;
const LABEL: u16 = 0x0204;
const BOOLERR: u16 = 0x0205;
const FORMULA_3: u16 = 0x0206; // BIFF3
const STRING: u16 = 0x0207;
const BOF_3: u16 = 0x0209;
const INDEX: u16 = 0x020B;
const NAME_3: u16 = 0x0218; // BIFF3, BIFF4
const ARRAY: u16 = 0x0221;
const TABLE: u16 = 0x0236;
const EXTERNNAME_3: u16 = 0x0223; // BIFF3, BIFF4
const XF_3: u16 = 0x0243; // BIFF3
const RK: u16 = 0x027E;
const FORMULA_4: u16 = 0x0406; // BIFF4
const BOF_4: u16 = 0x0409;
const FORMAT: u16 = 0x041E; // BIFF4, BIFF5, BIFF8
const XF_4: u16 = 0x0443; // BIFF4
const SHRFMLA: u16 = 0x04BC;
const BOF: u16 = 0x0809; // BIFF5, BIFF8

/// A name for a record type, for error messages.
fn record_name(typ: u16) -> &'static str {
    match typ {
        DIMENSIONS_2 | DIMENSIONS => "DIMENSIONS",
        BLANK_2 | BLANK => "BLANK",
        INTEGER_2 => "INTEGER",
        NUMBER_2 | NUMBER => "NUMBER",
        LABEL_2 | LABEL => "LABEL",
        BOOLERR_2 | BOOLERR => "BOOLERR",
        FORMULA | FORMULA_3 | FORMULA_4 => "FORMULA",
        STRING_2 | STRING => "STRING",
        BOF_2 | BOF_3 | BOF_4 | BOF => "BOF",
        EOF => "EOF",
        EXTERNSHEET => "EXTERNSHEET",
        NAME | NAME_3 => "NAME",
        FORMAT_2 | FORMAT => "FORMAT",
        ARRAY_2 | ARRAY => "ARRAY",
        DATEMODE => "DATEMODE",
        EXTERNNAME | EXTERNNAME_3 => "EXTERNNAME",
        COLWIDTH_2 => "COLWIDTH",
        FILEPASS => "FILEPASS",
        CODEPAGE => "CODEPAGE",
        XF_2 | XF | XF_3 | XF_4 => "XF",
        IXFE_2 => "IXFE",
        COLINFO => "COLINFO",
        BOUNDSHEET => "BOUNDSHEET",
        BUNDLESHEET => "BUNDLESHEET",
        MULRK => "MULRK",
        MULBLANK => "MULBLANK",
        SHRFMLA | SHRFMLA_OLD => "SHRFMLA",
        RSTRING => "RSTRING",
        MERGEDCELLS => "MERGEDCELLS",
        SST => "SST",
        LABELSST => "LABELSST",
        SUPBOOK => "SUPBOOK",
        RK => "RK",
        _ => "record",
    }
}

/// Number formats and XFs in scope for a substream.
#[derive(Default, Clone)]
struct Styles {
    formats: Vec<(u16, String)>,
    xf_formats: Vec<u16>,
}

impl Styles {
    fn format_text(&self, id: u16, version: BiffVersion) -> &str {
        if let Some((_, s)) = self.formats.iter().rev().find(|(i, _)| *i == id) {
            return s;
        }
        if version >= BiffVersion::Biff5
            && let Some(s) = formats::builtin_format(id)
        {
            return s;
        }
        "General"
    }
}

/// A NAME record before its formula is rendered.
struct NameRec {
    name: String,
    builtin: Option<u8>,
    sheet: Option<u16>,
    hidden: bool,
    function: bool,
    key: u8,
    flags: u16,
    extern_index: u16,
    text_lens: [u8; 4],
    tokens: Vec<u8>,
    extra: Vec<u8>,
}

/// A BOUNDSHEET or BUNDLESHEET entry.
struct BoundSheet {
    name: String,
    offset: u32,
    visibility: Visibility,
    kind: SheetKind,
}

/// The format of a cell while its substream is being read.
#[derive(Clone, Copy)]
enum PendingFormat {
    /// From the cell's XF (BIFF3 and later).
    FromXf,
    /// From the cell's own attributes (BIFF2).
    Id(u16),
}

struct SheetState {
    cells: Vec<Cell>,
    formats: Vec<PendingFormat>,
    /// Index of the last FORMULA cell whose string result is in the next
    /// STRING record.
    pending_string: Option<usize>,
    /// XF index from a BIFF2 IXFE record, for the next cell.
    ixfe: Option<u16>,
    dimensions: Option<CellRange>,
    merged: Vec<CellRange>,
    columns: Vec<ColumnInfo>,
    shared: Vec<SharedFormula>,
    arrays: Vec<ArrayFormula>,
    tables: Vec<DataTable>,
    raw: Vec<RawRecord>,
    styles: Styles,
    bof_kind: Option<SheetKind>,
}

impl SheetState {
    fn new() -> Self {
        SheetState {
            cells: Vec::new(),
            formats: Vec::new(),
            pending_string: None,
            ixfe: None,
            dimensions: None,
            merged: Vec::new(),
            columns: Vec::new(),
            shared: Vec::new(),
            arrays: Vec::new(),
            tables: Vec::new(),
            raw: Vec::new(),
            styles: Styles::default(),
            bof_kind: None,
        }
    }
}

struct Parser<'a> {
    stream: &'a [u8],
    limits: &'a Limits,
    version: BiffVersion,
    enc: &'static Encoding,
    codepage: Option<u16>,
    records: usize,
    cells: usize,
    warnings: Vec<Warning>,
    dropped_warnings: usize,
    date_1904: bool,
    styles: Styles,
    names: Vec<NameRec>,
    sst: Vec<SharedString>,
    bound: Vec<BoundSheet>,
    xti: Vec<Xti>,
    supbooks: Vec<SupBook>,
    extern_sheets: Vec<ExternSheet>,
    extern_names: Vec<Vec<String>>,
    raw: Vec<RawRecord>,
    interned: HashMap<String, u32>,
    number_formats: Vec<String>,
    rendered_names: Vec<FormulaText>,
    /// CONTINUE records the last handler consumed, for record numbering.
    consumed: u32,
    /// Bytes copied out of the stream and kept (raw records, formula
    /// tokens), and the cap on them.
    retained: usize,
    max_retained: usize,
    /// Substreams already read, start to end, so that two BOUNDSHEETs
    /// pointing at one substream cannot make us read it twice.
    walked: BTreeMap<usize, usize>,
}

/// Reads a workbook stream.
pub(crate) fn parse(
    stream: &[u8],
    container: Container,
    limits: &Limits,
) -> Result<BiffWorkbook, BiffError> {
    let mut recs = Records::new(stream, 0);
    let Some(first) = recs.next_rec()? else {
        return Err(BiffError::new(
            BiffErrorKind::NotXls {
                detail: "the workbook stream is empty".into(),
            },
            Some(0),
        ));
    };
    let (version, dt) = bof_version(&first)?;
    let mut p = Parser {
        stream,
        limits,
        version,
        enc: encoding_rs::WINDOWS_1252,
        codepage: None,
        records: 0,
        cells: 0,
        warnings: Vec::new(),
        dropped_warnings: 0,
        date_1904: false,
        styles: Styles::default(),
        names: Vec::new(),
        sst: Vec::new(),
        bound: Vec::new(),
        xti: Vec::new(),
        supbooks: Vec::new(),
        extern_sheets: Vec::new(),
        extern_names: Vec::new(),
        raw: Vec::new(),
        interned: HashMap::new(),
        number_formats: Vec::new(),
        rendered_names: Vec::new(),
        consumed: 0,
        retained: 0,
        // Every byte is kept at most once (raw or as formula tokens), so
        // twice the stream is a generous cap that still stops runaway copies.
        max_retained: stream.len().saturating_mul(2).saturating_add(1 << 16),
        walked: BTreeMap::new(),
    };
    p.intern("General");
    p.prescan_codepage()?;

    let biff4_workbook = version == BiffVersion::Biff4 && dt == 0x0100;
    let mut sheets = Vec::new();
    if version >= BiffVersion::Biff5 && dt != 0x0005 {
        // A stream that starts with a sheet, not the globals: a bare
        // BIFF5/8 sheet. Read it as a one-sheet workbook.
        p.warn(
            0,
            "the stream starts with a sheet, not the workbook globals".into(),
        );
    }
    if (version >= BiffVersion::Biff5 && dt == 0x0005) || biff4_workbook {
        let mut globals = SheetState::new();
        let end = p.walk(0, &mut globals)?;
        p.walked.insert(0, end);
        p.raw = std::mem::take(&mut globals.raw);
        p.styles = std::mem::take(&mut globals.styles);
        if !globals.cells.is_empty() {
            p.warn(
                0,
                "cell records in the workbook globals were ignored".into(),
            );
        }
        if p.bound.len() > limits.max_sheets {
            return Err(BiffError::limit("sheets", limits.max_sheets, None));
        }
        let bound = std::mem::take(&mut p.bound);
        let mut next = end;
        for (i, b) in bound.iter().enumerate() {
            let start = p.sheet_start(b, next, i);
            let Some(start) = start else {
                p.warn(
                    b.offset as u64,
                    format!("sheet \"{}\" has no BOF record; it is left empty", b.name),
                );
                sheets.push(empty_sheet(b));
                continue;
            };
            if p.already_walked(start) {
                p.warn(
                    b.offset as u64,
                    format!(
                        "sheet \"{}\" points to offset {start}, inside a substream already read; it is left empty",
                        b.name
                    ),
                );
                sheets.push(empty_sheet(b));
                continue;
            }
            let mut st = SheetState::new();
            next = p.walk(start, &mut st)?;
            p.walked.insert(start, next);
            sheets.push(p.finish_sheet(st, b.name.clone(), b.kind, b.visibility, start as u64));
        }
        p.bound = bound;
    } else {
        // BIFF2 to BIFF4 single-sheet file, or a bare sheet: the one
        // substream holds the globals too.
        let mut st = SheetState::new();
        p.walk(0, &mut st)?;
        p.styles = st.styles.clone();
        let kind = st.bof_kind.unwrap_or(SheetKind::Worksheet);
        sheets.push(p.finish_sheet(st, "Sheet1".into(), kind, Visibility::Visible, 0));
    }

    p.render_formulas(&mut sheets)?;
    let names = p.finish_names();

    if p.dropped_warnings > 0 {
        let n = p.dropped_warnings;
        p.warnings.push(Warning {
            offset: 0,
            message: format!("{n} more warnings were not kept"),
        });
    }
    Ok(BiffWorkbook {
        version,
        container,
        biff4_workbook,
        codepage: p.codepage,
        date_1904: p.date_1904,
        sheets,
        names,
        number_formats: p.number_formats,
        formats: p.styles.formats,
        xf_formats: p.styles.xf_formats,
        shared_strings: p.sst,
        raw_records: p.raw,
        warnings: p.warnings,
    })
}

fn empty_sheet(b: &BoundSheet) -> BiffSheet {
    BiffSheet {
        name: b.name.clone(),
        kind: b.kind,
        visibility: b.visibility,
        cells: Vec::new(),
        dimensions: None,
        merged: Vec::new(),
        columns: Vec::new(),
        shared_formulas: Vec::new(),
        array_formulas: Vec::new(),
        data_tables: Vec::new(),
        raw_records: Vec::new(),
        offset: b.offset as u64,
    }
}

/// BIFF version and substream type from a BOF record.
fn bof_version(rec: &Rec<'_>) -> Result<(BiffVersion, u16), BiffError> {
    let mut rd = Rd::new(rec.data, rec.typ, "BOF", rec.body_offset());
    let vers = rd.u16().unwrap_or(0);
    let dt = rd.u16().unwrap_or(0x0010);
    let version = match rec.typ {
        BOF_2 => BiffVersion::Biff2,
        BOF_3 => BiffVersion::Biff3,
        BOF_4 => BiffVersion::Biff4,
        BOF => match vers {
            0x0600 => BiffVersion::Biff8,
            0x0500 => BiffVersion::Biff5,
            // Some writers leave the version out or write odd values
            // (0x0580 from an Excel 5 for the Mac); BIFF8 BOFs are 16 bytes.
            _ if rec.data.len() >= 16 => BiffVersion::Biff8,
            0..=0x05FF => BiffVersion::Biff5,
            _ => {
                return Err(BiffError::at(
                    BiffErrorKind::UnsupportedVersion {
                        bof_type: rec.typ,
                        version: vers,
                    },
                    rec.offset,
                ));
            }
        },
        other => {
            return Err(BiffError::at(
                BiffErrorKind::NotXls {
                    detail: format!(
                        "the stream starts with record 0x{other:04X} instead of a BOF record"
                    ),
                },
                rec.offset,
            ));
        }
    };
    Ok((version, dt))
}

fn sheet_kind(dt: u16) -> SheetKind {
    match dt {
        0x0010 => SheetKind::Worksheet,
        0x0020 => SheetKind::Chart,
        0x0040 => SheetKind::MacroSheet,
        0x0006 => SheetKind::VbModule,
        other => SheetKind::Other(other),
    }
}

fn is_bof(typ: u16) -> bool {
    matches!(typ, BOF_2 | BOF_3 | BOF_4 | BOF)
}

impl<'a> Parser<'a> {
    /// Counts `n` more bytes kept from the stream.
    fn retain(&mut self, n: usize, offset: u64) -> Result<(), BiffError> {
        self.retained = self.retained.saturating_add(n);
        if self.retained > self.max_retained {
            return Err(BiffError::limit(
                "bytes kept from the stream (raw records and formula tokens)",
                self.max_retained,
                Some(offset),
            ));
        }
        Ok(())
    }

    fn keep_raw(
        &mut self,
        st: &mut SheetState,
        rec: &Rec<'_>,
        index: u32,
    ) -> Result<(), BiffError> {
        self.retain(rec.data.len(), rec.offset)?;
        st.raw.push(RawRecord {
            record_type: rec.typ,
            index,
            offset: rec.offset,
            data: rec.data.to_vec(),
        });
        Ok(())
    }

    fn keep_raw_parts(
        &mut self,
        st: &mut SheetState,
        parts: &[Rec<'_>],
        index: u32,
    ) -> Result<(), BiffError> {
        for (i, p) in parts.iter().enumerate() {
            self.keep_raw(st, p, index.saturating_add(i as u32))?;
        }
        Ok(())
    }

    /// True when `pos` lies inside a substream already read.
    fn already_walked(&self, pos: usize) -> bool {
        self.walked
            .range(..=pos)
            .next_back()
            .is_some_and(|(_, &end)| pos < end)
    }

    fn warn(&mut self, offset: u64, message: String) {
        if self.warnings.len() < self.limits.max_warnings {
            self.warnings.push(Warning { offset, message });
        } else {
            self.dropped_warnings += 1;
        }
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.interned.get(s) {
            return i;
        }
        let i = self.number_formats.len() as u32;
        self.number_formats.push(s.to_string());
        self.interned.insert(s.to_string(), i);
        i
    }

    fn decode(&self, bytes: &[u8]) -> String {
        codepage::decode(self.enc, bytes)
    }

    fn count_record(&mut self, offset: u64) -> Result<(), BiffError> {
        self.records += 1;
        if self.records > self.limits.max_records {
            return Err(BiffError::limit(
                "records",
                self.limits.max_records,
                Some(offset),
            ));
        }
        Ok(())
    }

    /// Finds the CODEPAGE record of the first substream before anything
    /// else, so that sheet names are decoded right even when it comes late.
    fn prescan_codepage(&mut self) -> Result<(), BiffError> {
        let mut recs = Records::new(self.stream, 0);
        let mut seen = 0usize;
        while let Some(r) = recs.next_rec()? {
            seen += 1;
            if seen > self.limits.max_records {
                return Err(BiffError::limit(
                    "records",
                    self.limits.max_records,
                    Some(r.offset),
                ));
            }
            if r.typ == CODEPAGE && r.data.len() >= 2 {
                self.set_codepage(u16::from_le_bytes([r.data[0], r.data[1]]), r.offset);
                break;
            }
            if r.typ == EOF {
                break;
            }
        }
        Ok(())
    }

    fn set_codepage(&mut self, cp: u16, offset: u64) {
        if self.codepage.is_some() {
            return;
        }
        self.codepage = Some(cp);
        if self.version == BiffVersion::Biff8 {
            // BIFF8 text is UTF-16 whatever the code page says.
            return;
        }
        match codepage::encoding_for(cp) {
            Some(e) => self.enc = e,
            None => self.warn(
                offset,
                format!("code page {cp} is not supported; text is read as windows-1252"),
            ),
        }
    }

    /// Where a sheet's substream starts: its BOUNDSHEET offset if a BOF is
    /// there, else right after the previous substream.
    fn sheet_start(&mut self, b: &BoundSheet, next: usize, i: usize) -> Option<usize> {
        let at = usize::try_from(b.offset).unwrap_or(usize::MAX);
        let bof_at = |pos: usize| -> bool {
            self.stream
                .get(pos..pos.saturating_add(2))
                .map(|t| is_bof(u16::from_le_bytes([t[0], t[1]])))
                .unwrap_or(false)
        };
        if bof_at(at) {
            return Some(at);
        }
        if bof_at(next) {
            self.warn(
                b.offset as u64,
                format!(
                    "sheet {} (\"{}\") points to offset {} where there is no BOF; using the next substream at {}",
                    i + 1,
                    b.name,
                    b.offset,
                    next
                ),
            );
            return Some(next);
        }
        None
    }

    /// Reads one substream from its BOF to the matching EOF. Returns the
    /// stream position after it.
    fn walk(&mut self, start: usize, st: &mut SheetState) -> Result<usize, BiffError> {
        let mut recs = Records::new(self.stream, start);
        let mut index: u32 = 0;
        let mut depth = 0usize;
        loop {
            let Some(rec) = recs.next_rec()? else {
                self.warn(
                    recs.pos() as u64,
                    "the stream ends without an EOF record".into(),
                );
                break;
            };
            self.count_record(rec.offset)?;
            let this_index = index;
            index = index.saturating_add(1);
            if this_index == 0 {
                if !is_bof(rec.typ) {
                    return Err(BiffError::at(
                        BiffErrorKind::Malformed {
                            record: "BOF",
                            record_type: rec.typ,
                            detail: "a substream does not start with a BOF record".into(),
                        },
                        rec.offset,
                    ));
                }
                let dt = rec.data.get(2..4).map(|b| u16::from_le_bytes([b[0], b[1]]));
                st.bof_kind = dt.map(sheet_kind);
                self.keep_raw(st, &rec, this_index)?;
                continue;
            }
            if depth > 0 {
                // An embedded substream (a chart in a worksheet): keep it all.
                if is_bof(rec.typ) {
                    depth += 1;
                } else if rec.typ == EOF {
                    depth -= 1;
                }
                self.keep_raw(st, &rec, this_index)?;
                continue;
            }
            if is_bof(rec.typ) {
                depth = 1;
                self.keep_raw(st, &rec, this_index)?;
                continue;
            }
            if rec.typ == EOF {
                break;
            }
            self.consumed = 0;
            self.record(rec, &mut recs, st, this_index)?;
            index = index.saturating_add(self.consumed);
        }
        Ok(recs.pos())
    }

    /// Handles one record of a substream (not BOF or EOF).
    fn record(
        &mut self,
        rec: Rec<'a>,
        recs: &mut Records<'a>,
        st: &mut SheetState,
        index: u32,
    ) -> Result<(), BiffError> {
        let v = self.version;
        let b2 = v == BiffVersion::Biff2;
        let rd = || Rd::new(rec.data, rec.typ, record_name(rec.typ), rec.body_offset());
        // Any cell record ends the wait for a formula's STRING record.
        let is_cell = matches!(
            rec.typ,
            BLANK
                | NUMBER
                | LABEL
                | BOOLERR
                | RK
                | MULRK
                | MULBLANK
                | RSTRING
                | LABELSST
                | FORMULA_3
                | FORMULA_4
        ) || (b2
            && matches!(
                rec.typ,
                BLANK_2 | INTEGER_2 | NUMBER_2 | LABEL_2 | BOOLERR_2
            ))
            || rec.typ == FORMULA;
        if is_cell {
            st.pending_string = None;
        }
        match rec.typ {
            FILEPASS => return Err(encrypted(&rec, v)),
            CODEPAGE => {
                let cp = rd().u16()?;
                self.set_codepage(cp, rec.offset);
            }
            DATEMODE => self.date_1904 = rd().u16()? == 1,
            FORMAT_2 if v <= BiffVersion::Biff3 => {
                let mut r = rd();
                let cch = r.u8()? as usize;
                let s = self.decode(r.take(cch.min(r.remaining()))?);
                let id = st.styles.formats.len() as u16;
                st.styles.formats.push((id, s));
            }
            FORMAT if v >= BiffVersion::Biff4 => {
                let parts = self.continues(rec, recs)?;
                let order = st.styles.formats.len() as u16;
                let (id, text) = match self.format_record(parts.clone(), order) {
                    Ok(f) => f,
                    Err(e) => {
                        // A broken format string is not worth losing the
                        // workbook over: keep the bytes, show General.
                        self.warn(
                            rec.offset,
                            format!("a FORMAT record was skipped: {}", e.why()),
                        );
                        self.keep_raw_parts(st, &parts, index)?;
                        let id = crate::stream::bytes::le_u16(rec.data, 0).unwrap_or(order);
                        (
                            if v == BiffVersion::Biff4 { order } else { id },
                            "General".to_string(),
                        )
                    }
                };
                st.styles.formats.push((id, text));
            }
            XF_2 if b2 => {
                let mut r = rd();
                r.skip(2)?;
                let ifmt = (r.u8()? & 0x3F) as u16;
                st.styles.xf_formats.push(ifmt);
                self.keep_raw(st, &rec, index)?;
            }
            XF_3 | XF_4 if matches!(v, BiffVersion::Biff3 | BiffVersion::Biff4) => {
                let mut r = rd();
                r.skip(1)?;
                let ifmt = r.u8()? as u16;
                st.styles.xf_formats.push(ifmt);
                self.keep_raw(st, &rec, index)?;
            }
            XF if v >= BiffVersion::Biff5 => {
                let mut r = rd();
                r.skip(2)?;
                let ifmt = r.u16()?;
                st.styles.xf_formats.push(ifmt);
                self.keep_raw(st, &rec, index)?;
            }
            NAME | NAME_3 => {
                if (rec.typ == NAME_3) != matches!(v, BiffVersion::Biff3 | BiffVersion::Biff4) {
                    self.keep_raw(st, &rec, index)?;
                    return Ok(());
                }
                if self.names.len() >= self.limits.max_names {
                    return Err(BiffError::limit(
                        "defined names",
                        self.limits.max_names,
                        Some(rec.offset),
                    ));
                }
                let parts = self.continues(rec, recs)?;
                let name = self.name_record(parts)?;
                self.names.push(name);
            }
            SST if v == BiffVersion::Biff8 => {
                let parts = self.continues(rec, recs)?;
                self.sst_record(parts)?;
            }
            EXTSST | INDEX | DBCELL => {}
            INDEX_2 if v == BiffVersion::Biff2 => {}
            BOUNDSHEET if v >= BiffVersion::Biff5 => {
                if self.bound.len() >= self.limits.max_sheets {
                    return Err(BiffError::limit(
                        "sheets",
                        self.limits.max_sheets,
                        Some(rec.offset),
                    ));
                }
                let b = self.boundsheet(&rec)?;
                self.bound.push(b);
            }
            BUNDLESHEET if v == BiffVersion::Biff4 => {
                if self.bound.len() >= self.limits.max_sheets {
                    return Err(BiffError::limit(
                        "sheets",
                        self.limits.max_sheets,
                        Some(rec.offset),
                    ));
                }
                let mut r = rd();
                let offset = r.u32()?;
                let _grbit = r.u16()?;
                let cch = r.u8()? as usize;
                let name = self.decode(r.take(cch.min(r.remaining()))?);
                self.bound.push(BoundSheet {
                    name,
                    offset,
                    visibility: Visibility::Visible,
                    kind: SheetKind::Worksheet,
                });
            }
            EXTERNSHEET => {
                let parts = self.continues(rec, recs)?;
                self.keep_raw_parts(st, &parts, index)?;
                self.externsheet(parts)?;
            }
            SUPBOOK if v == BiffVersion::Biff8 => {
                let parts = self.continues(rec, recs)?;
                self.keep_raw_parts(st, &parts, index)?;
                self.supbook(parts);
            }
            EXTERNNAME | EXTERNNAME_3 => {
                self.externname(&rec);
                self.keep_raw(st, &rec, index)?;
            }
            _ => self.sheet_record(rec, recs, st, index)?,
        }
        Ok(())
    }

    /// Collects a record with its CONTINUE records.
    fn continues(
        &mut self,
        rec: Rec<'a>,
        recs: &mut Records<'a>,
    ) -> Result<Vec<Rec<'a>>, BiffError> {
        let parts = record::with_continues(
            rec,
            recs,
            self.limits.max_continues,
            &mut self.records,
            self.limits.max_records,
        )?;
        self.consumed = self.consumed.saturating_add(parts.len() as u32 - 1);
        Ok(parts)
    }

    /// Cell and other sheet records.
    fn sheet_record(
        &mut self,
        rec: Rec<'a>,
        recs: &mut Records<'a>,
        st: &mut SheetState,
        index: u32,
    ) -> Result<(), BiffError> {
        let v = self.version;
        let b2 = v == BiffVersion::Biff2;
        let name = record_name(rec.typ);
        let mut r = Rd::new(rec.data, rec.typ, name, rec.body_offset());
        match rec.typ {
            DIMENSIONS_2 if b2 => st.dimensions = dimensions(&mut r, false)?,
            DIMENSIONS => {
                st.dimensions = dimensions(&mut r, v == BiffVersion::Biff8 && rec.data.len() >= 14)?
            }
            BLANK_2 if b2 => {
                let (row, col, xf, fmt) = self.cell_head2(&mut r, st)?;
                self.push(st, &rec, row, col, xf, fmt, CellValue::Empty, None)?;
            }
            INTEGER_2 if b2 => {
                let (row, col, xf, fmt) = self.cell_head2(&mut r, st)?;
                let n = r.u16()?;
                self.push(
                    st,
                    &rec,
                    row,
                    col,
                    xf,
                    fmt,
                    CellValue::Number(n as f64),
                    None,
                )?;
            }
            NUMBER_2 if b2 => {
                let (row, col, xf, fmt) = self.cell_head2(&mut r, st)?;
                let n = r.f64()?;
                self.push(st, &rec, row, col, xf, fmt, CellValue::Number(n), None)?;
            }
            LABEL_2 if b2 => {
                let (row, col, xf, fmt) = self.cell_head2(&mut r, st)?;
                let cch = r.u8()? as usize;
                let s = self.decode(r.take(cch)?);
                self.push(st, &rec, row, col, xf, fmt, CellValue::Text(s.into()), None)?;
            }
            BOOLERR_2 if b2 => {
                let (row, col, xf, fmt) = self.cell_head2(&mut r, st)?;
                let value = boolerr(&mut r)?;
                self.push(st, &rec, row, col, xf, fmt, value, None)?;
            }
            FORMULA if b2 => {
                let (row, col, xf, fmt) = self.cell_head2(&mut r, st)?;
                let value = r.take(8)?;
                let flags = r.u8()? as u16;
                // The token length is one byte in the BIFF2 layout of the
                // "Excel File Format" document; Apache POI reads two. Take
                // whichever makes the record length come out exact (16
                // header bytes, the length field, then the tokens).
                let len = rec.data.len();
                let one = rec.data.get(16).map(|&c| c as usize);
                let two = crate::stream::bytes::le_u16(rec.data, 16).map(|c| c as usize);
                let cce = match (one, two) {
                    (Some(c), _) if len == 17 + c => r.u8()? as usize,
                    (_, Some(c)) if len == 18 + c => r.u16()? as usize,
                    _ => r.u8()? as usize,
                };
                self.formula_cell(st, &rec, &mut r, row, col, xf, fmt, value, flags, cce)?;
            }
            BLANK => {
                let (row, col, xf) = cell_head(&mut r)?;
                self.push(
                    st,
                    &rec,
                    row,
                    col,
                    xf,
                    PendingFormat::FromXf,
                    CellValue::Empty,
                    None,
                )?;
            }
            NUMBER => {
                let (row, col, xf) = cell_head(&mut r)?;
                let n = r.f64()?;
                self.push(
                    st,
                    &rec,
                    row,
                    col,
                    xf,
                    PendingFormat::FromXf,
                    CellValue::Number(n),
                    None,
                )?;
            }
            RK => {
                let (row, col, xf) = cell_head(&mut r)?;
                let n = rk(r.u32()?);
                self.push(
                    st,
                    &rec,
                    row,
                    col,
                    xf,
                    PendingFormat::FromXf,
                    CellValue::Number(n),
                    None,
                )?;
            }
            BOOLERR => {
                let (row, col, xf) = cell_head(&mut r)?;
                let value = boolerr(&mut r)?;
                self.push(st, &rec, row, col, xf, PendingFormat::FromXf, value, None)?;
            }
            LABEL => {
                let (row, col, xf) = cell_head(&mut r)?;
                let cch = r.u16()? as usize;
                let s = if v == BiffVersion::Biff8 {
                    let mut c = CRd::single(r.rest(), rec.typ, name, r.offset());
                    read_unicode(&mut c, cch)?.0
                } else {
                    self.decode(r.take(cch)?)
                };
                self.push(
                    st,
                    &rec,
                    row,
                    col,
                    xf,
                    PendingFormat::FromXf,
                    CellValue::Text(s.into()),
                    None,
                )?;
            }
            RSTRING => {
                let (row, col, xf) = cell_head(&mut r)?;
                let cch = r.u16()? as usize;
                let (s, runs) = if v == BiffVersion::Biff8 {
                    let mut c = CRd::single(r.rest(), rec.typ, name, r.offset());
                    let (s, _, _) = read_unicode(&mut c, cch)?;
                    let n = c.u16().unwrap_or(0) as usize;
                    let mut runs = Vec::new();
                    for _ in 0..n.min(c.remaining() / 4) {
                        runs.push(FormatRun {
                            start: c.u16()?,
                            font: c.u16()?,
                        });
                    }
                    (s, runs)
                } else {
                    let s = self.decode(r.take(cch)?);
                    let n = if r.is_empty() { 0 } else { r.u8()? as usize };
                    let mut runs = Vec::new();
                    for _ in 0..n.min(r.remaining() / 2) {
                        runs.push(FormatRun {
                            start: r.u8()? as u16,
                            font: r.u8()? as u16,
                        });
                    }
                    (s, runs)
                };
                self.push(
                    st,
                    &rec,
                    row,
                    col,
                    xf,
                    PendingFormat::FromXf,
                    CellValue::Text(s.into()),
                    None,
                )?;
                if let Some(c) = st.cells.last_mut()
                    && !runs.is_empty()
                {
                    c.runs = Some(runs.into());
                }
            }
            LABELSST if v == BiffVersion::Biff8 => {
                let (row, col, xf) = cell_head(&mut r)?;
                let isst = r.u32()? as usize;
                let (value, runs) = match self.sst.get(isst) {
                    Some(s) => (CellValue::Text(s.text.clone()), s.runs.clone()),
                    None => {
                        self.warn(
                            rec.offset,
                            format!(
                                "LABELSST points to shared string {isst}, but the SST has {}",
                                self.sst.len()
                            ),
                        );
                        (CellValue::Empty, None)
                    }
                };
                self.push(st, &rec, row, col, xf, PendingFormat::FromXf, value, None)?;
                if let Some(c) = st.cells.last_mut() {
                    c.runs = runs;
                }
            }
            MULRK => {
                let row = r.u16()? as u32;
                let first = r.u16()?;
                if r.remaining() < 2 {
                    return Err(r.truncated(2));
                }
                let n = (r.remaining() - 2) / 6;
                for i in 0..n {
                    let xf = r.u16()?;
                    let value = rk(r.u32()?);
                    let col = first.saturating_add(i as u16);
                    self.push(
                        st,
                        &rec,
                        row,
                        col,
                        xf,
                        PendingFormat::FromXf,
                        CellValue::Number(value),
                        None,
                    )?;
                }
            }
            MULBLANK => {
                let row = r.u16()? as u32;
                let first = r.u16()?;
                if r.remaining() < 2 {
                    return Err(r.truncated(2));
                }
                let n = (r.remaining() - 2) / 2;
                for i in 0..n {
                    let xf = r.u16()?;
                    let col = first.saturating_add(i as u16);
                    self.push(
                        st,
                        &rec,
                        row,
                        col,
                        xf,
                        PendingFormat::FromXf,
                        CellValue::Empty,
                        None,
                    )?;
                }
            }
            FORMULA | FORMULA_3 | FORMULA_4 => {
                let expected = match v {
                    BiffVersion::Biff3 => FORMULA_3,
                    BiffVersion::Biff4 => FORMULA_4,
                    _ => FORMULA,
                };
                if rec.typ != expected {
                    self.keep_raw(st, &rec, index)?;
                    return Ok(());
                }
                let (row, col, xf) = cell_head(&mut r)?;
                let value = r.take(8)?;
                let flags = r.u16()?;
                if v >= BiffVersion::Biff5 {
                    r.skip(4)?;
                }
                let cce = r.u16()? as usize;
                self.formula_cell(
                    st,
                    &rec,
                    &mut r,
                    row,
                    col,
                    xf,
                    PendingFormat::FromXf,
                    value,
                    flags,
                    cce,
                )?;
            }
            STRING_2 | STRING if rec.typ == STRING || b2 => {
                let s = if rec.typ == STRING_2 {
                    let cch = r.u8()? as usize;
                    self.decode(r.take(cch.min(r.remaining()))?)
                } else if v == BiffVersion::Biff8 {
                    let parts = self.continues(rec, recs)?;
                    let mut c = CRd::new(parts, "STRING");
                    let cch = c.u16()? as usize;
                    read_unicode(&mut c, cch)?.0
                } else {
                    let cch = r.u16()? as usize;
                    self.decode(r.take(cch.min(r.remaining()))?)
                };
                match st.pending_string.take() {
                    Some(i) => {
                        if let Some(c) = st.cells.get_mut(i) {
                            c.value = CellValue::Text(s.into());
                        }
                    }
                    None => self.warn(rec.offset, "a STRING record follows no formula".into()),
                }
            }
            SHRFMLA | SHRFMLA_OLD if v >= BiffVersion::Biff5 => {
                let range = ref_u(&mut r)?;
                let reserved = r.u8()?;
                let uses = r.u8()?;
                let cce = r.u16()? as usize;
                let tokens = r.take(cce)?.to_vec();
                let extra = r.rest().to_vec();
                self.retain(tokens.len() + extra.len(), rec.offset)?;
                st.shared.push(SharedFormula {
                    range,
                    reserved,
                    uses,
                    tokens,
                    extra,
                });
            }
            ARRAY_2 | ARRAY if rec.typ == ARRAY || b2 => {
                let range = ref_u(&mut r)?;
                let (flags, cce) = if b2 {
                    (r.u8()? as u16, r.u8()? as usize)
                } else {
                    let flags = r.u16()?;
                    // BIFF5 and BIFF8 have a 4-byte field here. BIFF3/4
                    // files are read with whichever layout fits.
                    let fits_short = matches!(v, BiffVersion::Biff3 | BiffVersion::Biff4)
                        && r.remaining() >= 2
                        && crate::stream::bytes::le_u16(r.rest(), 0)
                            .map(|c| c as usize + 2 == r.remaining())
                            == Some(true);
                    if !fits_short {
                        r.skip(4)?;
                    }
                    (flags, r.u16()? as usize)
                };
                let tokens = r.take(cce)?.to_vec();
                let extra = r.rest().to_vec();
                self.retain(tokens.len() + extra.len(), rec.offset)?;
                st.arrays.push(ArrayFormula {
                    range,
                    flags,
                    tokens,
                    extra,
                    text: FormulaText::Unparsed {
                        reason: "pending".into(),
                        at: 0,
                    },
                });
            }
            TABLE if v >= BiffVersion::Biff3 => {
                let range = ref_u(&mut r)?;
                let flags = r.u16()?;
                let r1 = r.u16()? as u32;
                let c1 = r.u16()?;
                let r2 = r.u16()? as u32;
                let c2 = r.u16()?;
                let first = if flags & 0x10 != 0 {
                    "#REF!".to_string()
                } else {
                    cell_name(r1, c1)
                };
                let second = if flags & 0x20 != 0 {
                    "#REF!".to_string()
                } else {
                    cell_name(r2, c2)
                };
                let text = if flags & 0x08 != 0 {
                    format!("TABLE({first},{second})")
                } else if flags & 0x04 != 0 {
                    format!("TABLE({first},)")
                } else {
                    format!("TABLE(,{first})")
                };
                st.tables.push(DataTable { range, text });
                self.keep_raw(st, &rec, index)?;
            }
            MERGEDCELLS if v == BiffVersion::Biff8 => {
                let n = r.u16()? as usize;
                let fit = r.remaining() / 8;
                if n > fit {
                    self.warn(
                        rec.offset,
                        format!("MERGEDCELLS declares {n} ranges but holds {fit}"),
                    );
                }
                let mut inverted = 0;
                for _ in 0..n.min(fit) {
                    let r1 = r.u16()? as u32;
                    let r2 = r.u16()? as u32;
                    let c1 = r.u16()?;
                    let c2 = r.u16()?;
                    if r1 <= r2 && c1 <= c2 {
                        st.merged.push(CellRange {
                            first_row: r1,
                            first_col: c1,
                            last_row: r2,
                            last_col: c2,
                        });
                    } else {
                        inverted += 1;
                    }
                }
                if inverted > 0 {
                    self.warn(
                        rec.offset,
                        format!(
                            "MERGEDCELLS holds {inverted} ranges whose end comes before their start; the record is kept as stored"
                        ),
                    );
                }
                if inverted > 0 || n > fit {
                    // Keep the record as stored, so nothing is lost.
                    self.keep_raw(st, &rec, index)?;
                }
            }
            COLINFO if v >= BiffVersion::Biff3 => {
                let first_col = r.u16()?;
                let last_col = r.u16()?;
                let width = r.u16()?;
                let xf = r.u16()?;
                let flags = r.u16()?;
                st.columns.push(ColumnInfo {
                    first_col,
                    last_col,
                    width,
                    xf,
                    hidden: flags & 1 != 0,
                    outline_level: ((flags >> 8) & 7) as u8,
                    collapsed: flags & 0x1000 != 0,
                    flags,
                });
                if !r.is_empty() {
                    // Keep the reserved tail too.
                    self.keep_raw(st, &rec, index)?;
                }
            }
            COLWIDTH_2 if b2 => {
                let first_col = r.u8()? as u16;
                let last_col = r.u8()? as u16;
                let width = r.u16()?;
                st.columns.push(ColumnInfo {
                    first_col,
                    last_col,
                    width,
                    xf: 0,
                    hidden: false,
                    outline_level: 0,
                    collapsed: false,
                    flags: 0,
                });
            }
            IXFE_2 if b2 => st.ixfe = Some(r.u16()?),
            _ => self.keep_raw(st, &rec, index)?,
        }
        Ok(())
    }

    /// Row, column and attributes of a BIFF2 cell record.
    fn cell_head2(
        &mut self,
        r: &mut Rd<'_>,
        st: &mut SheetState,
    ) -> Result<(u32, u16, u16, PendingFormat), BiffError> {
        let row = r.u16()? as u32;
        let col = r.u16()?;
        let attr = r.take(3)?;
        let mut xf = (attr[0] & 0x3F) as u16;
        if xf == 63 {
            xf = st.ixfe.take().unwrap_or(xf);
        }
        let ifmt = (attr[1] & 0x3F) as u16;
        Ok((row, col, xf, PendingFormat::Id(ifmt)))
    }

    #[allow(clippy::too_many_arguments)]
    fn formula_cell(
        &mut self,
        st: &mut SheetState,
        rec: &Rec<'_>,
        r: &mut Rd<'_>,
        row: u32,
        col: u16,
        xf: u16,
        fmt: PendingFormat,
        value: &[u8],
        flags: u16,
        cce: usize,
    ) -> Result<(), BiffError> {
        let tokens = r.take(cce)?.to_vec();
        let extra = r.rest().to_vec();
        self.retain(tokens.len() + extra.len(), rec.offset)?;
        let (value, wants_string) = cached_value(value);
        let formula = Formula {
            text: FormulaText::Unparsed {
                reason: "pending".into(),
                at: 0,
            },
            kind: FormulaKind::Normal,
            tokens,
            extra,
            flags,
        };
        let pushed = self.push(st, rec, row, col, xf, fmt, value, Some(Box::new(formula)))?;
        if wants_string && pushed {
            st.pending_string = Some(st.cells.len() - 1);
        }
        Ok(())
    }

    /// Adds a cell. Returns false if the cell was skipped.
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        st: &mut SheetState,
        rec: &Rec<'_>,
        row: u32,
        col: u16,
        xf: u16,
        fmt: PendingFormat,
        value: CellValue,
        formula: Option<Box<Formula>>,
    ) -> Result<bool, BiffError> {
        if col > self.limits.max_col {
            self.warn(
                rec.offset,
                format!(
                    "a cell in column {} is beyond the last column; it was skipped",
                    col as u32 + 1
                ),
            );
            return Ok(false);
        }
        self.cells += 1;
        if self.cells > self.limits.max_cells {
            return Err(BiffError::limit(
                "cells",
                self.limits.max_cells,
                Some(rec.offset),
            ));
        }
        st.cells.push(Cell {
            row,
            col,
            xf,
            format: 0,
            value,
            formula,
            runs: None,
        });
        st.formats.push(fmt);
        Ok(true)
    }

    /// A BIFF4 to BIFF8 FORMAT record: (format id, format string). BIFF4
    /// numbers formats by position (`order`).
    fn format_record(
        &mut self,
        parts: Vec<Rec<'a>>,
        order: u16,
    ) -> Result<(u16, String), BiffError> {
        let offset = parts[0].offset;
        let mut r = CRd::new(parts, "FORMAT");
        let raw_id = r.u16()?;
        Ok(match self.version {
            BiffVersion::Biff4 | BiffVersion::Biff5 => {
                let cch = r.u8()? as usize;
                let b = r.bytes(cch.min(r.remaining()))?;
                let id = if self.version == BiffVersion::Biff4 {
                    order
                } else {
                    raw_id
                };
                (id, self.decode(&b))
            }
            _ => {
                let cch = r.u16()? as usize;
                let flags = r.u8()?;
                let width = if flags & 1 != 0 { 2 } else { 1 };
                let fits = r.remaining() / width;
                // Some writers cut the last character of a format string;
                // keep what is there.
                let cch = if fits < cch && flags & 0x0C == 0 {
                    self.warn(
                        offset,
                        format!("a FORMAT string declares {cch} characters but holds {fits}"),
                    );
                    fits
                } else {
                    cch
                };
                (raw_id, read_unicode_body(&mut r, cch, flags)?.0)
            }
        })
    }

    fn name_record(&mut self, parts: Vec<Rec<'a>>) -> Result<NameRec, BiffError> {
        let v = self.version;
        let mut r = CRd::new(parts, "NAME");
        let flags = r.u16()?;
        let key = r.u8()?;
        let cch = r.u8()? as usize;
        let builtin_flag = flags & 0x0020 != 0;
        let mut extern_index = 0;
        let mut text_lens = [0u8; 4];
        let (cce, sheet) = match v {
            BiffVersion::Biff2 => (r.u8()? as usize, None),
            BiffVersion::Biff3 | BiffVersion::Biff4 => (r.u16()? as usize, None),
            _ => {
                let cce = r.u16()? as usize;
                extern_index = r.u16()?;
                let itab = r.u16()?;
                for len in text_lens.iter_mut() {
                    *len = r.u8()?;
                }
                (cce, itab.checked_sub(1))
            }
        };
        let raw_name = if v == BiffVersion::Biff8 {
            let flag = r.u8()?;
            if flag & 0xFE != 0 {
                return Err(r.malformed(format!("name option byte 0x{flag:02X} is not 0 or 1")));
            }
            let width = if flag & 1 != 0 { 2 } else { 1 };
            let b = r.bytes(cch * width)?;
            if width == 2 {
                codepage::utf16le(&b)
            } else {
                codepage::latin1(&b)
            }
        } else {
            let b = r.bytes(cch)?;
            self.decode(&b)
        };
        let builtin = if builtin_flag {
            raw_name
                .chars()
                .next()
                .map(|c| c as u32)
                .filter(|&c| c < 256)
                .map(|c| c as u8)
        } else {
            None
        };
        let name = match builtin {
            Some(code) => builtin_name(code)
                .map(str::to_string)
                .unwrap_or_else(|| format!("_builtin{code}")),
            None => raw_name,
        };
        let tokens = r.bytes(cce)?;
        let extra = r.bytes(r.remaining())?;
        Ok(NameRec {
            name,
            builtin,
            sheet,
            hidden: flags & 1 != 0,
            function: flags & 0x0002 != 0,
            key,
            flags,
            extern_index,
            text_lens,
            tokens,
            extra,
        })
    }

    fn sst_record(&mut self, parts: Vec<Rec<'a>>) -> Result<(), BiffError> {
        let mut r = CRd::new(parts, "SST");
        let _total = r.u32()?;
        let unique = r.u32()? as usize;
        // Each string takes at least 3 bytes; never trust the count alone.
        let cap = unique
            .min(r.remaining() / 3)
            .min(self.limits.max_shared_strings);
        let mut out = Vec::with_capacity(cap);
        // Empty strings share one allocation (a hostile SST can hold
        // millions of them in 3 bytes each).
        let empty: Arc<str> = Arc::from("");
        while out.len() < unique {
            if r.is_done() {
                self.warn(
                    r.offset(),
                    format!("the SST declares {unique} strings but holds {}", out.len()),
                );
                break;
            }
            if out.len() >= self.limits.max_shared_strings {
                return Err(BiffError::limit(
                    "shared strings",
                    self.limits.max_shared_strings,
                    Some(r.offset()),
                ));
            }
            let cch = r.u16()? as usize;
            let (text, runs, phonetic) = read_unicode(&mut r, cch)?;
            out.push(SharedString {
                text: if text.is_empty() {
                    empty.clone()
                } else {
                    text.into()
                },
                runs: if runs.is_empty() {
                    None
                } else {
                    Some(runs.into())
                },
                phonetic,
            });
        }
        self.sst = out;
        Ok(())
    }

    fn boundsheet(&mut self, rec: &Rec<'_>) -> Result<BoundSheet, BiffError> {
        let mut r = Rd::new(rec.data, rec.typ, "BOUNDSHEET", rec.body_offset());
        let offset = r.u32()?;
        let state = r.u8()?;
        let dt = r.u8()?;
        let cch = r.u8()? as usize;
        let name = if self.version == BiffVersion::Biff8 {
            let mut c = CRd::single(r.rest(), rec.typ, "BOUNDSHEET", r.offset());
            let flag = c.u8()?;
            c.chars(cch, flag & 1 != 0)?
        } else {
            // Some writers put a BIFF8-style option byte here; skip it
            // when the name would not fit otherwise (as Apache POI does).
            if r.remaining() == cch + 1 && r.rest().first() == Some(&0) {
                r.skip(1)?;
            }
            let b = r.take(cch)?;
            self.decode(b)
        };
        let visibility = match state & 3 {
            0 => Visibility::Visible,
            1 => Visibility::Hidden,
            _ => Visibility::VeryHidden,
        };
        let kind = match dt {
            0 => SheetKind::Worksheet,
            1 => SheetKind::MacroSheet,
            2 => SheetKind::Chart,
            6 => SheetKind::VbModule,
            other => SheetKind::Other(other as u16),
        };
        Ok(BoundSheet {
            name,
            offset,
            visibility,
            kind,
        })
    }

    fn externsheet(&mut self, parts: Vec<Rec<'a>>) -> Result<(), BiffError> {
        let mut r = CRd::new(parts, "EXTERNSHEET");
        if self.version == BiffVersion::Biff8 {
            let n = r.u16()? as usize;
            let fit = r.remaining() / 6;
            for _ in 0..n.min(fit) {
                let supbook = r.u16()?;
                let first = r.u16()? as i16;
                let last = r.u16()? as i16;
                self.xti.push(Xti {
                    supbook,
                    first,
                    last,
                });
            }
            return Ok(());
        }
        let cch = r.u8()? as usize;
        let b = r.bytes(cch.min(r.remaining()))?;
        let es = match b.first() {
            Some(0x02) | Some(0x03) => ExternSheet::Own(self.decode(&b[1..])),
            Some(0x04) => ExternSheet::OwnBook,
            Some(0x01) => {
                let (book, sheet) = decode_url(&self.decode(&b[1..]));
                ExternSheet::External { book, sheet }
            }
            Some(_) => {
                // A plain string: the name of a sheet in another book or a
                // DDE/OLE link.
                ExternSheet::External {
                    book: self.decode(&b),
                    sheet: String::new(),
                }
            }
            None => ExternSheet::Unknown,
        };
        self.extern_sheets.push(es);
        self.extern_names.push(Vec::new());
        Ok(())
    }

    fn supbook(&mut self, parts: Vec<Rec<'a>>) {
        let offset = parts[0].offset;
        let mut r = CRd::new(parts, "SUPBOOK");
        let kind = (|| -> Result<SupBookKind, BiffError> {
            let ctab = r.u16()? as usize;
            let cch = r.u16()?;
            Ok(match cch {
                0x0401 => SupBookKind::Internal,
                0x3A01 => SupBookKind::AddIn,
                n => {
                    let (path, _, _) = read_unicode(&mut r, n as usize)?;
                    let (book, _) = decode_url(&path);
                    let mut sheets = Vec::new();
                    for _ in 0..ctab {
                        if r.is_done() {
                            break;
                        }
                        let cch = r.u16()? as usize;
                        sheets.push(read_unicode(&mut r, cch)?.0);
                    }
                    SupBookKind::External { book, sheets }
                }
            })
        })();
        let kind = match kind {
            Ok(k) => k,
            Err(e) => {
                self.warn(
                    offset,
                    format!("a SUPBOOK record could not be read: {}", e.why()),
                );
                SupBookKind::Other
            }
        };
        self.supbooks.push(SupBook {
            kind,
            names: Vec::new(),
        });
    }

    fn externname(&mut self, rec: &Rec<'_>) {
        let mut r = Rd::new(rec.data, rec.typ, "EXTERNNAME", rec.body_offset());
        let name = match self.version {
            BiffVersion::Biff8 => (|| -> Result<String, BiffError> {
                r.skip(6)?;
                let cch = r.u8()? as usize;
                let mut c = CRd::single(r.rest(), rec.typ, "EXTERNNAME", r.offset());
                let flag = c.u8()?;
                c.chars(cch, flag & 1 != 0)
            })(),
            BiffVersion::Biff5 => (|| -> Result<String, BiffError> {
                r.skip(6)?;
                let cch = r.u8()? as usize;
                Ok(self.decode(r.take(cch)?))
            })(),
            _ => return,
        };
        let Ok(name) = name else {
            self.warn(rec.offset, "an EXTERNNAME record could not be read".into());
            return;
        };
        if self.version == BiffVersion::Biff8 {
            if let Some(b) = self.supbooks.last_mut() {
                b.names.push(name);
            }
        } else if let Some(n) = self.extern_names.last_mut() {
            n.push(name);
        }
    }

    /// Sorts the cells, resolves their number formats, and builds the sheet.
    fn finish_sheet(
        &mut self,
        mut st: SheetState,
        name: String,
        kind: SheetKind,
        visibility: Visibility,
        offset: u64,
    ) -> BiffSheet {
        let styles = if st.styles.xf_formats.is_empty() && st.styles.formats.is_empty() {
            self.styles.clone()
        } else {
            st.styles.clone()
        };
        let mut cells = std::mem::take(&mut st.cells);
        let mut cache: HashMap<u16, u32> = HashMap::new();
        for (cell, fmt) in cells.iter_mut().zip(st.formats.iter()) {
            let id = match fmt {
                PendingFormat::Id(id) => *id,
                PendingFormat::FromXf => styles
                    .xf_formats
                    .get(cell.xf as usize)
                    .copied()
                    .unwrap_or(0),
            };
            let idx = match cache.get(&id) {
                Some(&i) => i,
                None => {
                    let text = styles.format_text(id, self.version).to_string();
                    let i = self.intern(&text);
                    cache.insert(id, i);
                    i
                }
            };
            cell.format = idx;
        }
        // Stable sort, then keep the last record for each position.
        cells.sort_by_key(|c| (c.row, c.col));
        let mut deduped: Vec<Cell> = Vec::with_capacity(cells.len());
        for c in cells {
            if let Some(last) = deduped.last_mut()
                && last.row == c.row
                && last.col == c.col
            {
                *last = c;
                continue;
            }
            deduped.push(c);
        }
        let kind = match (st.bof_kind, kind) {
            (Some(SheetKind::Chart), _) => SheetKind::Chart,
            (Some(SheetKind::MacroSheet), _) => SheetKind::MacroSheet,
            (_, k) => k,
        };
        BiffSheet {
            name,
            kind,
            visibility,
            cells: deduped,
            dimensions: st.dimensions,
            merged: st.merged,
            columns: st.columns,
            shared_formulas: st.shared,
            array_formulas: st.arrays,
            data_tables: st.tables,
            raw_records: st.raw,
            offset,
        }
    }

    fn name_texts(&self) -> Vec<String> {
        self.names.iter().map(|n| n.name.clone()).collect()
    }

    /// Renders every formula once all records are read, so that names,
    /// sheets and external references are known.
    fn render_formulas(&mut self, sheets: &mut [BiffSheet]) -> Result<(), BiffError> {
        let sheet_names: Vec<String> = sheets.iter().map(|s| s.name.clone()).collect();
        let names = self.name_texts();
        let env = Env {
            version: self.version,
            sheet_names: &sheet_names,
            names: &names,
            xti: &self.xti,
            supbooks: &self.supbooks,
            extern_sheets: &self.extern_sheets,
            extern_names: &self.extern_names,
            enc: self.enc,
            max_text: self.limits.max_formula_text,
        };
        let mut budget = Budget {
            text: 0,
            max_text: self.limits.max_total_formula_text,
            work: 0,
            max_work: self
                .stream
                .len()
                .saturating_mul(self.limits.max_formula_work_per_byte)
                .saturating_add(16 << 20),
            lookups: 0,
            max_lookups: self.limits.max_formula_lookups,
            lookups_spent: false,
        };
        for sheet in sheets.iter_mut() {
            for a in sheet.array_formulas.iter_mut() {
                budget.work(a.tokens.len() + a.extra.len())?;
                a.text = formula::render(
                    &a.tokens,
                    &a.extra,
                    (a.range.first_row, a.range.first_col),
                    &env,
                );
                budget.text(&a.text)?;
            }
            let defs = Defs::new(
                &sheet.shared_formulas,
                &sheet.array_formulas,
                &sheet.data_tables,
            );
            for cell in sheet.cells.iter_mut() {
                let Some(f) = cell.formula.as_mut() else {
                    continue;
                };
                let (text, kind) = resolve(
                    f,
                    cell.row,
                    cell.col,
                    &defs,
                    &env,
                    self.version,
                    &mut budget,
                )?;
                f.text = text;
                f.kind = kind;
                budget.text(&f.text)?;
            }
        }
        // Names are rendered here too; they are attached in finish_names.
        let mut rendered = Vec::with_capacity(self.names.len());
        for n in &self.names {
            budget.work(n.tokens.len() + n.extra.len())?;
            let t = formula::render(&n.tokens, &n.extra, (0, 0), &env);
            budget.text(&t)?;
            rendered.push(t);
        }
        self.rendered_names = rendered;
        if budget.lookups_spent {
            self.warn(
                0,
                format!(
                    "more than {} range checks were needed to match cells to shared, array and table formulas; the cells not matched by then are left unparsed",
                    self.limits.max_formula_lookups
                ),
            );
        }
        Ok(())
    }

    fn finish_names(&mut self) -> Vec<DefinedName> {
        let rendered = std::mem::take(&mut self.rendered_names);
        std::mem::take(&mut self.names)
            .into_iter()
            .zip(rendered)
            .map(|(n, text)| DefinedName {
                name: n.name,
                builtin: n.builtin,
                sheet: n.sheet,
                hidden: n.hidden,
                function: n.function,
                key: n.key,
                flags: n.flags,
                extern_index: n.extern_index,
                text_lens: n.text_lens,
                formula: Formula {
                    text,
                    kind: FormulaKind::Normal,
                    tokens: n.tokens,
                    extra: n.extra,
                    flags: n.flags,
                },
            })
            .collect()
    }
}

/// What rendering formulas may cost: text written, token bytes read (a
/// shared formula counts once for every cell that uses it), and range
/// checks while looking for a cell's shared, array or table formula.
struct Budget {
    text: usize,
    max_text: usize,
    work: usize,
    max_work: usize,
    lookups: usize,
    max_lookups: usize,
    lookups_spent: bool,
}

impl Budget {
    fn text(&mut self, t: &FormulaText) -> Result<(), BiffError> {
        if let FormulaText::Parsed(s) = t {
            self.text = self.text.saturating_add(s.len());
            if self.text > self.max_text {
                return Err(BiffError::limit(
                    "total formula text in bytes",
                    self.max_text,
                    None,
                ));
            }
        }
        Ok(())
    }

    fn work(&mut self, token_bytes: usize) -> Result<(), BiffError> {
        self.work = self.work.saturating_add(token_bytes);
        if self.work > self.max_work {
            return Err(BiffError::limit(
                "formula token bytes rendered (shared formulas count once per cell)",
                self.max_work,
                None,
            ));
        }
        Ok(())
    }

    /// Charges `n` range checks; false once the budget is spent (the
    /// search is then skipped and the formula left unparsed).
    fn lookups(&mut self, n: usize) -> bool {
        self.lookups = self.lookups.saturating_add(n);
        if self.lookups > self.max_lookups {
            self.lookups_spent = true;
            return false;
        }
        true
    }
}

type Anchors = HashMap<(u32, u16), Vec<usize>>;

/// A sheet's shared, array and table formula definitions, indexed by the
/// top-left cell of their range.
struct Defs<'s> {
    shared: &'s [SharedFormula],
    arrays: &'s [ArrayFormula],
    tables: &'s [DataTable],
    shared_at: Anchors,
    arrays_at: Anchors,
    tables_at: Anchors,
}

impl<'s> Defs<'s> {
    fn new(
        shared: &'s [SharedFormula],
        arrays: &'s [ArrayFormula],
        tables: &'s [DataTable],
    ) -> Self {
        fn index<'a>(ranges: impl Iterator<Item = &'a CellRange>) -> Anchors {
            let mut m: Anchors = HashMap::new();
            for (i, r) in ranges.enumerate() {
                m.entry((r.first_row, r.first_col)).or_default().push(i);
            }
            m
        }
        Defs {
            shared,
            arrays,
            tables,
            shared_at: index(shared.iter().map(|s| &s.range)),
            arrays_at: index(arrays.iter().map(|a| &a.range)),
            tables_at: index(tables.iter().map(|t| &t.range)),
        }
    }
}

/// The last definition anchored at `anchor` whose range holds the cell,
/// else (for writers that anchor elsewhere or let ranges overlap) the last
/// one whose range holds both the anchor and the cell. Every range check is
/// charged to the budget, so hostile files cannot make this quadratic.
fn find_def(
    len: usize,
    range: impl Fn(usize) -> CellRange,
    at: &Anchors,
    anchor: (u32, u16),
    cell: (u32, u16),
    budget: &mut Budget,
) -> Option<usize> {
    if let Some(list) = at.get(&anchor) {
        if !budget.lookups(list.len()) {
            return None;
        }
        if let Some(&i) = list
            .iter()
            .rev()
            .find(|&&i| range(i).contains(cell.0, cell.1))
        {
            return Some(i);
        }
    }
    if !budget.lookups(len) {
        return None;
    }
    (0..len).rev().find(|&i| {
        let r = range(i);
        r.contains(anchor.0, anchor.1) && r.contains(cell.0, cell.1)
    })
}

/// Works out a cell formula's text, following PtgExp and PtgTbl to the
/// shared, array or table formula they point at.
fn resolve(
    f: &Formula,
    row: u32,
    col: u16,
    defs: &Defs<'_>,
    env: &Env<'_>,
    version: BiffVersion,
    budget: &mut Budget,
) -> Result<(FormulaText, FormulaKind), BiffError> {
    let first = f.tokens.first().copied();
    if first != Some(0x01) && first != Some(0x02) {
        budget.work(f.tokens.len() + f.extra.len())?;
        return Ok((
            formula::render(&f.tokens, &f.extra, (row, col), env),
            FormulaKind::Normal,
        ));
    }
    let t = &f.tokens[1..];
    let anchor = match t.len() {
        n if n >= 4 && version >= BiffVersion::Biff5 => Some((
            u16::from_le_bytes([t[0], t[1]]) as u32,
            u16::from_le_bytes([t[2], t[3]]),
        )),
        n if n >= 3 => Some((u16::from_le_bytes([t[0], t[1]]) as u32, t[2] as u16)),
        _ => None,
    };
    let Some((ar, ac)) = anchor else {
        return Ok((
            FormulaText::Unparsed {
                reason: "a shared formula reference is too short".into(),
                at: 0,
            },
            FormulaKind::Normal,
        ));
    };
    let cell = (row, col);
    if first == Some(0x02) {
        let tables = defs.tables;
        let mut hit = find_def(
            tables.len(),
            |i| tables[i].range,
            &defs.tables_at,
            (ar, ac),
            cell,
            budget,
        );
        if hit.is_none() && budget.lookups(tables.len()) {
            // Take any table that holds the cell.
            hit = tables.iter().rposition(|t| t.range.contains(row, col));
        }
        let text = match hit {
            Some(i) => {
                budget.work(tables[i].text.len())?;
                FormulaText::Parsed(tables[i].text.clone())
            }
            None => FormulaText::Unparsed {
                reason: format!(
                    "points to a data table at {} that does not exist",
                    cell_name(ar, ac)
                ),
                at: 0,
            },
        };
        let kind = FormulaKind::Table {
            anchor_row: ar,
            anchor_col: ac,
        };
        return Ok((text, kind));
    }
    let shared = defs.shared;
    if let Some(i) = find_def(
        shared.len(),
        |i| shared[i].range,
        &defs.shared_at,
        (ar, ac),
        cell,
        budget,
    ) {
        let s = &shared[i];
        budget.work(s.tokens.len() + s.extra.len())?;
        return Ok((
            formula::render_shared(&s.tokens, &s.extra, cell, env),
            FormulaKind::Shared {
                anchor_row: ar,
                anchor_col: ac,
            },
        ));
    }
    let arrays = defs.arrays;
    if let Some(i) = find_def(
        arrays.len(),
        |i| arrays[i].range,
        &defs.arrays_at,
        (ar, ac),
        cell,
        budget,
    ) {
        let a = &arrays[i];
        budget.work(a.text.as_str().map_or(0, str::len))?;
        return Ok((a.text.clone(), FormulaKind::Array { range: a.range }));
    }
    Ok((
        FormulaText::Unparsed {
            reason: if budget.lookups_spent {
                format!(
                    "points to a shared or array formula at {} that was not found within the lookup limit",
                    cell_name(ar, ac)
                )
            } else {
                format!(
                    "points to a shared or array formula at {} that does not exist",
                    cell_name(ar, ac)
                )
            },
            at: 0,
        },
        FormulaKind::Normal,
    ))
}

fn encrypted(rec: &Rec<'_>, v: BiffVersion) -> BiffError {
    let method = if v == BiffVersion::Biff8 {
        match crate::stream::bytes::le_u16(rec.data, 0) {
            Some(0) => Encryption::Xor,
            Some(1) => match crate::stream::bytes::le_u16(rec.data, 2) {
                Some(1) => Encryption::Rc4,
                Some(2..=4) => Encryption::Rc4CryptoApi,
                _ => Encryption::Unknown,
            },
            _ => Encryption::Unknown,
        }
    } else {
        Encryption::Xor
    };
    BiffError::at(BiffErrorKind::Encrypted { method }, rec.offset)
}

fn cell_head(r: &mut Rd<'_>) -> Result<(u32, u16, u16), BiffError> {
    Ok((r.u16()? as u32, r.u16()?, r.u16()?))
}

fn boolerr(r: &mut Rd<'_>) -> Result<CellValue, BiffError> {
    let v = r.u8()?;
    let is_err = r.u8()?;
    Ok(if is_err != 0 {
        CellValue::Error(CellError::from_code(v))
    } else {
        CellValue::Bool(v != 0)
    })
}

/// Decodes an RK number ([MS-XLS] 2.5.217).
pub(crate) fn rk(v: u32) -> f64 {
    let n = if v & 2 != 0 {
        ((v as i32) >> 2) as f64
    } else {
        f64::from_bits(((v & 0xFFFF_FFFC) as u64) << 32)
    };
    if v & 1 != 0 { n / 100.0 } else { n }
}

/// The cached result of a FORMULA record, and whether a STRING record
/// with the text follows.
fn cached_value(b: &[u8]) -> (CellValue, bool) {
    if b.len() == 8 && b[6] == 0xFF && b[7] == 0xFF {
        match b[0] {
            0 => (CellValue::Text("".into()), true),
            1 => (CellValue::Bool(b[2] != 0), false),
            2 => (CellValue::Error(CellError::from_code(b[2])), false),
            3 => (CellValue::Empty, false),
            _ => (CellValue::Empty, false),
        }
    } else {
        let mut a = [0u8; 8];
        a.copy_from_slice(&b[..8.min(b.len())]);
        (CellValue::Number(f64::from_le_bytes(a)), false)
    }
}

fn dimensions(r: &mut Rd<'_>, wide: bool) -> Result<Option<CellRange>, BiffError> {
    let (r1, r2) = if wide {
        (r.u32()?, r.u32()?)
    } else {
        (r.u16()? as u32, r.u16()? as u32)
    };
    let c1 = r.u16()?;
    let c2 = r.u16()?;
    if r2 <= r1 || c2 <= c1 {
        return Ok(None);
    }
    Ok(Some(CellRange {
        first_row: r1,
        first_col: c1,
        last_row: r2 - 1,
        last_col: c2 - 1,
    }))
}

/// A RefU: first row, last row, first column, last column (8-bit columns).
fn ref_u(r: &mut Rd<'_>) -> Result<CellRange, BiffError> {
    let r1 = r.u16()? as u32;
    let r2 = r.u16()? as u32;
    let c1 = r.u8()? as u16;
    let c2 = r.u8()? as u16;
    Ok(CellRange {
        first_row: r1.min(r2),
        first_col: c1.min(c2),
        last_row: r1.max(r2),
        last_col: c1.max(c2),
    })
}

/// Reads the rest of a BIFF8 XLUnicodeRichExtendedString after its
/// character count: option byte, run count, phonetic size, characters,
/// runs and phonetic data.
fn read_unicode(
    r: &mut CRd<'_>,
    cch: usize,
) -> Result<(String, Vec<FormatRun>, Vec<u8>), BiffError> {
    let flags = r.u8()?;
    read_unicode_body(r, cch, flags)
}

/// [`read_unicode`] after the option byte.
fn read_unicode_body(
    r: &mut CRd<'_>,
    cch: usize,
    flags: u8,
) -> Result<(String, Vec<FormatRun>, Vec<u8>), BiffError> {
    let runs = if flags & 0x08 != 0 {
        r.u16()? as usize
    } else {
        0
    };
    let ext = if flags & 0x04 != 0 {
        r.u32()? as usize
    } else {
        0
    };
    let text = r.chars(cch, flags & 1 != 0)?;
    if runs * 4 > r.remaining() {
        return Err(r.truncated(runs * 4));
    }
    let mut out = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = r.u16()?;
        let font = r.u16()?;
        out.push(FormatRun { start, font });
    }
    let phonetic = r.bytes(ext)?;
    Ok((text, out, phonetic))
}

/// Splits an encoded file name ([MS-XLS] 2.5.277 VirtualPath) into the book
/// file name and an optional sheet name.
fn decode_url(s: &str) -> (String, String) {
    let mut path = String::new();
    let mut chars = s.chars().peekable();
    if chars.peek() == Some(&'\u{1}') {
        chars.next();
    }
    while let Some(c) = chars.next() {
        match c {
            '\u{1}' => {
                // A drive letter follows; '@' means a UNC path.
                if let Some(d) = chars.next() {
                    if d == '@' {
                        path.push_str("\\\\");
                    } else {
                        path.push(d);
                        path.push_str(":\\");
                    }
                }
            }
            '\u{2}' | '\u{3}' => path.push('\\'),
            '\u{4}' => path.push_str("..\\"),
            '\u{5}' => {
                // A long volume such as a URL: a count, then the name.
                // The count is a character, so it can be huge; stop at the
                // end of the string.
                let n = chars.next().map(|c| c as usize).unwrap_or(0);
                for _ in 0..n.min(s.len()) {
                    match chars.next() {
                        Some(c) => path.push(c),
                        None => break,
                    }
                }
            }
            '\u{6}' | '\u{7}' | '\u{8}' => {}
            c => path.push(c),
        }
    }
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
    // A BIFF5 EXTERNSHEET may name the sheet after the file: "[book]sheet"
    // or "book\x03sheet" forms are both seen; keep it simple.
    match file.split_once(']') {
        Some((b, s)) => (b.trim_start_matches('[').to_string(), s.to_string()),
        None => (file, String::new()),
    }
}

fn builtin_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x00 => "Consolidate_Area",
        0x01 => "Auto_Open",
        0x02 => "Auto_Close",
        0x03 => "Extract",
        0x04 => "Database",
        0x05 => "Criteria",
        0x06 => "Print_Area",
        0x07 => "Print_Titles",
        0x08 => "Recorder",
        0x09 => "Data_Form",
        0x0A => "Auto_Activate",
        0x0B => "Auto_Deactivate",
        0x0C => "Sheet_Title",
        0x0D => "_FilterDatabase",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk_values() {
        // Integer 1, integer -1, 1.23 as /100 integer, and 1.0 as a float.
        assert_eq!(rk(0x0000_0006), 1.0);
        assert_eq!(rk(0xFFFF_FFFE), -1.0);
        assert_eq!(rk((123 << 2) | 3), 1.23);
        assert_eq!(rk(0x3FF0_0000), 1.0);
    }

    #[test]
    fn cached_values() {
        assert_eq!(
            cached_value(&1.5f64.to_le_bytes()).0,
            CellValue::Number(1.5)
        );
        assert_eq!(
            cached_value(&[1, 0, 1, 0, 0, 0, 0xFF, 0xFF]).0,
            CellValue::Bool(true)
        );
        assert_eq!(
            cached_value(&[2, 0, 0x07, 0, 0, 0, 0xFF, 0xFF]).0,
            CellValue::Error(CellError::Div0)
        );
        assert!(cached_value(&[0, 0, 0, 0, 0, 0, 0xFF, 0xFF]).1);
    }

    #[test]
    fn urls() {
        assert_eq!(
            decode_url("\u{1}\u{1}C\u{3}data\u{3}Book.xls"),
            ("Book.xls".to_string(), String::new())
        );
        let url = "http://example.org/Refs.xls";
        let encoded = format!("\u{1}\u{5}{}{url}", char::from(url.len() as u8));
        assert_eq!(decode_url(&encoded).0, "Refs.xls");
    }
}
