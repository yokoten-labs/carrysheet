//! Builds small workbooks in the test, so no large or hostile file is
//! committed: a minimal zip writer (stored entries, or raw deflate bytes
//! given by the test) and the few parts a workbook needs.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use carrysheet_commands::engine::run;
use carrysheet_commands::{COMMANDS, CommandError, validate_args};
use carrysheet_kernel::{Kernel, Limits};
use serde_json::{Map, Value};

pub const NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";

/// One zip entry: stored as is, or with raw deflate bytes already made.
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
    /// Raw deflate bytes that unpack to `data`; `None` stores `data`.
    pub deflated: Option<Vec<u8>>,
}

pub fn entry(name: &str, data: impl Into<Vec<u8>>) -> Entry {
    Entry {
        name: name.to_owned(),
        data: data.into(),
        deflated: None,
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

pub fn zip(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let (method, body): (u16, &[u8]) = match &e.deflated {
            Some(d) => (8, d),
            None => (0, &e.data),
        };
        let crc = crc32(&e.data);
        let offset = out.len() as u32;
        let name = e.name.as_bytes();
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        out.extend(le32(0x0403_4b50));
        out.extend(le16(20));
        out.extend(le16(0));
        out.extend(le16(method));
        out.extend(le32(0));
        out.extend(le32(crc));
        out.extend(le32(body.len() as u32));
        out.extend(le32(e.data.len() as u32));
        out.extend(le16(name.len() as u16));
        out.extend(le16(0));
        out.extend(name);
        out.extend(body);
        central.extend(le32(0x0201_4b50));
        central.extend(le16(20));
        central.extend(le16(20));
        central.extend(le16(0));
        central.extend(le16(method));
        central.extend(le32(0));
        central.extend(le32(crc));
        central.extend(le32(body.len() as u32));
        central.extend(le32(e.data.len() as u32));
        central.extend(le16(name.len() as u16));
        central.extend([0u8; 12]);
        central.extend(le32(offset));
        central.extend(name);
    }
    let cd_offset = out.len() as u32;
    let cd_len = central.len() as u32;
    out.extend(central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend(cd_len.to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend([0u8; 2]);
    out
}

/// Raw deflate: `n` empty fixed-Huffman blocks, then `data` in a final
/// stored block (at most 65,535 bytes).
pub fn empty_blocks(n: usize, data: &[u8]) -> Vec<u8> {
    struct Bits {
        bytes: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl Bits {
        fn put(&mut self, v: u32, len: u32) {
            self.acc |= v << self.n;
            self.n += len;
            while self.n >= 8 {
                self.bytes.push((self.acc & 0xFF) as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }
    }
    let mut w = Bits {
        bytes: Vec::with_capacity(n * 10 / 8 + data.len() + 8),
        acc: 0,
        n: 0,
    };
    for _ in 0..n {
        // Not final, fixed Huffman, end of block (seven zero bits).
        w.put(0, 1);
        w.put(1, 2);
        w.put(0, 7);
    }
    // The final block is stored, from the next whole byte.
    w.put(1, 1);
    w.put(0, 2);
    if w.n > 0 {
        let pad = 8 - w.n;
        w.put(0, pad);
    }
    let mut bytes = w.bytes;
    let len = data.len() as u16;
    bytes.extend(len.to_le_bytes());
    bytes.extend((!len).to_le_bytes());
    bytes.extend(data);
    bytes
}

/// The parts of a one-sheet workbook ("S"), with `extra` parts and
/// workbook relationships (type tail, target).
pub struct Book {
    pub sheet: String,
    pub workbook: String,
    pub styles: Option<String>,
    pub strings: Option<String>,
    pub extra: Vec<Entry>,
    pub workbook_rels: Vec<(String, String)>,
    pub sheet_rels: Vec<(String, String, bool)>,
    pub content_types: Vec<(String, String)>,
    /// Raw deflate bytes for the sheet part, instead of storing it.
    pub sheet_deflated: Option<Vec<u8>>,
}

impl Book {
    pub fn new(sheet_data: &str) -> Book {
        Book::with_sheet(&sheet_xml(sheet_data, "", ""))
    }

    pub fn with_sheet(sheet: &str) -> Book {
        Book {
            sheet: sheet.to_owned(),
            workbook: format!(
                r#"<?xml version="1.0"?><workbook {NS}><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#
            ),
            styles: None,
            strings: None,
            extra: vec![],
            workbook_rels: vec![],
            sheet_rels: vec![],
            content_types: vec![],
            sheet_deflated: None,
        }
    }

    pub fn bytes(self) -> Vec<u8> {
        let mut types = String::from(
            r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
        );
        for (part, ct) in &self.content_types {
            types.push_str(&format!(
                r#"<Override PartName="/{part}" ContentType="{ct}"/>"#
            ));
        }
        types.push_str("</Types>");
        let rels = format!(
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}officeDocument" Target="xl/workbook.xml"/></Relationships>"#
        );
        let mut wb_rels = format!(
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}worksheet" Target="worksheets/sheet1.xml"/>"#
        );
        let mut entries = vec![
            entry("[Content_Types].xml", types),
            entry("_rels/.rels", rels),
            entry("xl/workbook.xml", self.workbook),
        ];
        let mut n = 2;
        if let Some(s) = self.styles {
            wb_rels.push_str(&format!(
                r#"<Relationship Id="rId{n}" Type="{REL}styles" Target="styles.xml"/>"#
            ));
            n += 1;
            entries.push(entry("xl/styles.xml", s));
        }
        if let Some(s) = self.strings {
            wb_rels.push_str(&format!(
                r#"<Relationship Id="rId{n}" Type="{REL}sharedStrings" Target="sharedStrings.xml"/>"#
            ));
            n += 1;
            entries.push(entry("xl/sharedStrings.xml", s));
        }
        for (ty, target) in &self.workbook_rels {
            let ty = if ty.starts_with("http") {
                ty.clone()
            } else {
                format!("{REL}{ty}")
            };
            wb_rels.push_str(&format!(
                r#"<Relationship Id="rId{n}" Type="{ty}" Target="{target}"/>"#
            ));
            n += 1;
        }
        wb_rels.push_str("</Relationships>");
        entries.push(entry("xl/_rels/workbook.xml.rels", wb_rels));
        if !self.sheet_rels.is_empty() {
            let mut r = String::from(
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            );
            for (id, (ty, target, external)) in self.sheet_rels.iter().enumerate() {
                let ty = if ty.starts_with("http") {
                    ty.clone()
                } else {
                    format!("{REL}{ty}")
                };
                let mode = if *external {
                    r#" TargetMode="External""#
                } else {
                    ""
                };
                r.push_str(&format!(
                    r#"<Relationship Id="rIdS{}" Type="{ty}" Target="{target}"{mode}/>"#,
                    id + 1
                ));
            }
            r.push_str("</Relationships>");
            entries.push(entry("xl/worksheets/_rels/sheet1.xml.rels", r));
        }
        let mut sheet = entry("xl/worksheets/sheet1.xml", self.sheet);
        sheet.deflated = self.sheet_deflated;
        entries.push(sheet);
        entries.extend(self.extra);
        zip(&entries)
    }
}

/// A worksheet part: `before` and `after` go around `<sheetData>`.
pub fn sheet_xml(data: &str, before: &str, after: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><worksheet {NS}>{before}<sheetData>{data}</sheetData>{after}</worksheet>"#
    )
}

/// A fresh folder for one test, under the system's temporary folder.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("carrysheet-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn p(path: &Path) -> String {
    path.display().to_string()
}

pub fn call(name: &str, args: Value) -> Result<Map<String, Value>, CommandError> {
    call_with(&Kernel::new(), name, args)
}

pub fn call_with(
    kernel: &Kernel,
    name: &str,
    args: Value,
) -> Result<Map<String, Value>, CommandError> {
    let command = COMMANDS.iter().find(|c| c.name == name).unwrap();
    let args = validate_args(command, &args)?;
    run::run(kernel, command, &args)
}

pub fn lowered(f: impl FnOnce(&mut Limits)) -> Kernel {
    let mut limits = Limits::default();
    f(&mut limits);
    Kernel::new().with_limits(limits)
}

/// The text of a part of a saved package.
pub fn part_text(path: &Path, name: &str) -> String {
    use std::io::Read as _;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut s = String::new();
    zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}

pub fn lost(done: &Map<String, Value>) -> Vec<String> {
    done["save"]["lost"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["feature"].as_str().unwrap().to_owned())
        .collect()
}
