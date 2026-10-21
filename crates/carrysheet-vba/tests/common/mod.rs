//! Builds small VBA projects in code, so tests control every byte.
#![allow(dead_code)]

use carrysheet_vba::cfb::{Builder, Version};

pub fn data(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// A valid MS-OVBA container that stores `data` as literals only: every
/// chunk is a compressed chunk whose flag bytes are all zero.
pub fn compress_literal(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x01];
    // 3584 literals take 3584 + 448 flag bytes + 2 header bytes = 4034,
    // inside the 4098-byte chunk limit.
    for piece in data.chunks(3584) {
        let mut body = Vec::new();
        for group in piece.chunks(8) {
            body.push(0x00);
            body.extend_from_slice(group);
        }
        let size = body.len() + 2;
        let header = 0x8000 | 0x3000 | (size as u16 - 3);
        out.extend_from_slice(&header.to_le_bytes());
        out.extend_from_slice(&body);
    }
    out
}

/// A container whose chunks each expand 6 bytes to 4096: a decompression
/// bomb of `chunks` * 4096 bytes.
pub fn bomb(chunks: usize) -> Vec<u8> {
    let mut out = vec![0x01];
    for _ in 0..chunks {
        // Header: compressed, 4 bytes of data (size field = 6 - 3).
        out.extend_from_slice(&(0xB000u16 | 3).to_le_bytes());
        // Flag byte: token 0 literal, token 1 copy.
        out.push(0b10);
        out.push(b'a');
        // At 1 byte written: offset 1 (top 4 bits 0), length 4095 = 4092 + 3.
        out.extend_from_slice(&4092u16.to_le_bytes());
    }
    out
}

#[derive(Clone)]
pub struct ModuleSpec {
    pub name: String,
    /// The stream in `VBA/`; the name unless changed.
    pub stream: String,
    pub source: String,
    /// MODULETYPE 0x0021 when true.
    pub procedural: bool,
    /// The `PROJECT` stream line, such as `Module=Name`; `None` hides it.
    pub project_line: Option<String>,
    /// Filler bytes standing in for p-code before the source.
    pub pcode: usize,
    /// When false, the module stream holds p-code only.
    pub with_source: bool,
}

impl ModuleSpec {
    pub fn standard(name: &str, source: &str) -> Self {
        ModuleSpec {
            name: name.into(),
            stream: name.into(),
            source: source.into(),
            procedural: true,
            project_line: Some(format!("Module={name}")),
            pcode: 700,
            with_source: true,
        }
    }

    pub fn document(name: &str, source: &str) -> Self {
        ModuleSpec {
            name: name.into(),
            stream: name.into(),
            source: source.into(),
            procedural: false,
            project_line: Some(format!("Document={name}/&H00000000")),
            pcode: 800,
            with_source: true,
        }
    }
}

fn rec(out: &mut Vec<u8>, id: u16, data: &[u8]) {
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn mbcs(s: &str, code_page: u16) -> Vec<u8> {
    let (enc, _) = carrysheet_vba::encoding_for_code_page(code_page);
    enc.encode(s).0.into_owned()
}

/// The decompressed `dir` stream for these modules, per MS-OVBA 2.3.4.2.
pub fn dir_stream(code_page: u16, modules: &[ModuleSpec]) -> Vec<u8> {
    let mut d = Vec::new();
    rec(&mut d, 0x0001, &1u32.to_le_bytes()); // SYSKIND Win32
    rec(&mut d, 0x0002, &0x0409u32.to_le_bytes()); // LCID
    rec(&mut d, 0x0014, &0x0409u32.to_le_bytes()); // LCIDINVOKE
    rec(&mut d, 0x0003, &code_page.to_le_bytes()); // CODEPAGE
    rec(&mut d, 0x0004, b"VBAProject"); // NAME
    rec(&mut d, 0x0005, b""); // DOCSTRING
    rec(&mut d, 0x0040, b"");
    rec(&mut d, 0x0006, b""); // HELPFILEPATH
    rec(&mut d, 0x003D, b"");
    rec(&mut d, 0x0007, &0u32.to_le_bytes()); // HELPCONTEXT
    rec(&mut d, 0x0008, &0u32.to_le_bytes()); // LIBFLAGS
    // VERSION: reserved 4, major u32, minor u16.
    d.extend_from_slice(&0x0009u16.to_le_bytes());
    d.extend_from_slice(&4u32.to_le_bytes());
    d.extend_from_slice(&1_234_567u32.to_le_bytes());
    d.extend_from_slice(&5u16.to_le_bytes());
    rec(&mut d, 0x000C, b""); // CONSTANTS
    rec(&mut d, 0x003C, b"");
    // One registered reference.
    rec(&mut d, 0x0016, b"stdole");
    rec(&mut d, 0x003E, &utf16("stdole"));
    let libid = b"*\\G{00020430-0000-0000-C000-000000000046}#2.0#0#stdole2.tlb#OLE Automation";
    d.extend_from_slice(&0x000Du16.to_le_bytes());
    d.extend_from_slice(&(4 + libid.len() as u32 + 6).to_le_bytes());
    d.extend_from_slice(&(libid.len() as u32).to_le_bytes());
    d.extend_from_slice(libid);
    d.extend_from_slice(&[0; 6]);
    // Modules.
    rec(&mut d, 0x000F, &(modules.len() as u16).to_le_bytes());
    rec(&mut d, 0x0013, &0xFFFFu16.to_le_bytes());
    for m in modules {
        rec(&mut d, 0x0019, &mbcs(&m.name, code_page));
        rec(&mut d, 0x0047, &utf16(&m.name));
        rec(&mut d, 0x001A, &mbcs(&m.stream, code_page));
        rec(&mut d, 0x0032, &utf16(&m.stream));
        rec(&mut d, 0x001C, b"");
        rec(&mut d, 0x0048, b"");
        rec(&mut d, 0x0031, &(m.pcode as u32).to_le_bytes());
        rec(&mut d, 0x001E, &0u32.to_le_bytes());
        rec(&mut d, 0x002C, &0xFFFFu16.to_le_bytes());
        let kind: u16 = if m.procedural { 0x0021 } else { 0x0022 };
        d.extend_from_slice(&kind.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&0x002Bu16.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
    }
    d.extend_from_slice(&0x0010u16.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d
}

pub fn project_stream(code_page: u16, modules: &[ModuleSpec]) -> Vec<u8> {
    let mut s = String::from("ID=\"{00000000-0000-0000-0000-000000000000}\"\r\n");
    for m in modules {
        if let Some(line) = &m.project_line {
            s.push_str(line);
            s.push_str("\r\n");
        }
    }
    s.push_str(
        "Name=\"VBAProject\"\r\nHelpContextID=\"0\"\r\nVersionCompatible32=\"393222000\"\r\n",
    );
    s.push_str("\r\n[Host Extender Info]\r\n&H00000001={3832D640-CF90-11CF-8E43-00A0C911005A};VBE;&H00000000\r\n");
    mbcs(&s, code_page)
}

pub fn module_stream(m: &ModuleSpec, code_page: u16) -> Vec<u8> {
    let mut out: Vec<u8> = (0..m.pcode).map(|i| (i * 7 % 251) as u8).collect();
    if m.with_source {
        out.extend(compress_literal(&mbcs(&m.source, code_page)));
    }
    out
}

/// Every stream of a project, by path, before it goes into a compound file.
pub struct Streams(pub Vec<(String, Vec<u8>)>);

impl Streams {
    pub fn new(code_page: u16, modules: &[ModuleSpec]) -> Streams {
        let v = vec![
            ("PROJECT".to_string(), project_stream(code_page, modules)),
            (
                "VBA/dir".to_string(),
                compress_literal(&dir_stream(code_page, modules)),
            ),
            // _VBA_PROJECT header: reserved 0x61CC, version 0xFFFF, reserved.
            (
                "VBA/_VBA_PROJECT".to_string(),
                vec![0xCC, 0x61, 0xFF, 0xFF, 0x00, 0x00, 0x00],
            ),
        ];
        let mut s = Streams(v);
        for m in modules {
            let path = format!("VBA/{}", m.stream);
            if !s.0.iter().any(|(p, _)| *p == path) {
                s.0.push((path, module_stream(m, code_page)));
            }
        }
        s
    }

    pub fn set(&mut self, path: &str, bytes: Vec<u8>) {
        match self.0.iter_mut().find(|(p, _)| p == path) {
            Some(entry) => entry.1 = bytes,
            None => self.0.push((path.to_string(), bytes)),
        }
    }

    pub fn remove(&mut self, path: &str) {
        self.0.retain(|(p, _)| p != path);
    }

    /// A `vbaProject.bin`: the streams under the root storage.
    pub fn to_bin(&self) -> Vec<u8> {
        self.to_compound(None)
    }

    /// A bare `.xls`-like compound file: a `Workbook` stream and the
    /// project under `_VBA_PROJECT_CUR`.
    pub fn to_xls(&self) -> Vec<u8> {
        self.to_compound(Some("_VBA_PROJECT_CUR"))
    }

    fn to_compound(&self, storage: Option<&str>) -> Vec<u8> {
        let mut cf = Builder::new(Version::V3);
        let base = match storage {
            Some(s) => {
                cf.create_stream("/Workbook", vec![0x09, 0x08, 0, 0])
                    .unwrap();
                cf.create_storage(&format!("/{s}")).unwrap();
                format!("/{s}")
            }
            None => String::new(),
        };
        cf.create_storage(&format!("{base}/VBA")).unwrap();
        for (path, bytes) in &self.0 {
            cf.put_stream(&format!("{base}/{path}"), bytes.clone())
                .unwrap();
        }
        cf.to_bytes().unwrap()
    }
}

/// A small fixed-seed generator (xorshift64*), so mutation tests repeat.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    /// Flips, overwrites, inserts, deletes or truncates a few bytes.
    pub fn mutate(&mut self, bytes: &mut Vec<u8>) {
        let edits = 1 + self.below(4);
        for _ in 0..edits {
            if bytes.is_empty() {
                bytes.push(self.next() as u8);
                continue;
            }
            let at = self.below(bytes.len());
            match self.below(6) {
                0 => bytes[at] ^= 1 << self.below(8),
                1 => bytes[at] = self.next() as u8,
                2 => bytes[at] = [0x00, 0xFF, 0x7F, 0x80][self.below(4)],
                3 => bytes.insert(at, self.next() as u8),
                4 => {
                    bytes.remove(at);
                }
                _ => bytes.truncate(at),
            }
        }
    }
}
