//! Large and hostile inputs, made from a fixed seed at run time.
//!
//! Nothing here is committed as a file: the bench writes each input once
//! into its inputs folder (`target/bench-inputs/` by default) and reuses it.
//! The file name carries the seed and a generator version, so a changed
//! generator writes a new file instead of reusing a stale one. Bump
//! [`VERSION`] whenever a generator's output changes, and update the
//! baseline in the same commit.

use std::io::{self, Write};

/// The seed every generator starts from.
pub const SEED: u64 = 58;

/// Bumped when any generator's output changes.
pub const VERSION: u32 = 1;

/// A generated input: its file name and how to write it.
pub struct Generated {
    /// File name stem; the path adds the seed, version and `ext`.
    pub stem: &'static str,
    /// File extension, without the dot.
    pub ext: &'static str,
    /// Writes the input. Must give the same bytes for the same seed, unless
    /// `salted` is set.
    pub make: fn(&mut dyn Write) -> io::Result<()>,
    /// The bytes change on every generation (fresh salts, as encryption
    /// needs), so the input hash is not compared against the baseline.
    pub salted: bool,
}

impl Generated {
    /// The file name inside the inputs folder.
    pub fn file_name(&self) -> String {
        format!("{}-s{SEED}-v{VERSION}.{}", self.stem, self.ext)
    }
}

/// A small fixed-seed generator (xorshift64*), so inputs repeat exactly.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }

    fn word(&mut self, out: &mut String, lo: u64, hi: u64) {
        for _ in 0..self.range(lo, hi) {
            out.push((b'a' + self.below(26) as u8) as char);
        }
    }
}

const CITIES: &[&str] = &[
    "Zurich", "Osaka", "Lyon", "Taipei", "Austin", "Leeds", "Porto", "Busan", "Lagos", "Quito",
];

/// About 100 MB of CSV (100,000,000 bytes at most), eight columns of
/// typical business data: ids, names, cities, amounts, dates, booleans,
/// quoted notes with commas and doubled quotes, counts. About 880,000
/// rows, under Excel's 1,048,576-row limit and the reader's cell limit.
pub fn csv_100mb(w: &mut dyn Write) -> io::Result<()> {
    const TARGET: usize = 100_000_000;
    let mut rng = Rng::new(SEED);
    let header = "id,name,city,amount,date,active,note,count\r\n";
    w.write_all(header.as_bytes())?;
    let mut written = header.len();
    let mut row = String::new();
    for id in 1u64.. {
        row.clear();
        row.push_str(&id.to_string());
        row.push(',');
        rng.word(&mut row, 5, 12);
        row.push(',');
        row.push_str(rng.pick(CITIES));
        row.push(',');
        let cents = rng.below(10_000_000);
        row.push_str(&format!("{}.{:02}", cents / 100, cents % 100));
        row.push(',');
        row.push_str(&format!(
            "20{:02}-{:02}-{:02}",
            rng.range(10, 26),
            rng.range(1, 12),
            rng.range(1, 28)
        ));
        row.push(',');
        row.push_str(if rng.below(2) == 0 { "true" } else { "false" });
        row.push_str(",\"");
        let note_len = rng.range(30, 80) as usize;
        let start = row.len();
        while row.len() - start < note_len {
            rng.word(&mut row, 2, 9);
            match rng.below(12) {
                0 => row.push_str(", "),
                1 => row.push_str(" \"\"quoted\"\" "),
                _ => row.push(' '),
            }
        }
        row.push_str("\",");
        row.push_str(&rng.below(100_000).to_string());
        row.push_str("\r\n");
        if written + row.len() > TARGET {
            break;
        }
        w.write_all(row.as_bytes())?;
        written += row.len();
    }
    Ok(())
}

/// A wide CSV: 256 rows of 16,384 columns (Excel's column limit), short
/// numbers. About 4.2 million cells in 21 MB.
pub fn csv_wide(w: &mut dyn Write) -> io::Result<()> {
    let mut rng = Rng::new(SEED ^ 1);
    let mut row = String::new();
    for _ in 0..256 {
        row.clear();
        for col in 0..16_384 {
            if col > 0 {
                row.push(',');
            }
            row.push_str(&rng.below(100_000).to_string());
        }
        row.push_str("\r\n");
        w.write_all(row.as_bytes())?;
    }
    Ok(())
}

/// Hostile: 200,000 rows of four quoted fields full of doubled quotes,
/// delimiters and line breaks inside the quotes. About 26 MB.
pub fn csv_quotes(w: &mut dyn Write) -> io::Result<()> {
    let mut rng = Rng::new(SEED ^ 2);
    let mut row = String::new();
    for _ in 0..200_000 {
        row.clear();
        for col in 0..4 {
            if col > 0 {
                row.push(',');
            }
            row.push('"');
            for _ in 0..rng.range(3, 8) {
                rng.word(&mut row, 1, 4);
                row.push_str(match rng.below(4) {
                    0 => "\"\"",
                    1 => ",",
                    2 => "\r\n",
                    _ => "\"\",\"\"",
                });
            }
            row.push('"');
        }
        row.push_str("\r\n");
        w.write_all(row.as_bytes())?;
    }
    Ok(())
}

/// Hostile: 1,000,000 rows of nine empty fields, 9 million cells in 10 MB,
/// so per-cell bookkeeping is all the reader does.
pub fn csv_empty_cells(w: &mut dyn Write) -> io::Result<()> {
    let row = b",,,,,,,,\r\n";
    let block: Vec<u8> = row.repeat(10_000);
    for _ in 0..100 {
        w.write_all(&block)?;
    }
    Ok(())
}

const HANZI: &str = "的一是在不了有和人这中大为上个国我以要他时来用们生到作地于出就分对成会可主发年动同工也能下过子说产种面而方后多定行学法所民得经十三之进着等部度家电力里如水化高自二理起小物现实加量都两体制机当使点从业本去把性好应开它合还因由其些然前外天政四日那社义事平形相全表间样与关各重新线内数正心反你明看原又么利比或但质气第向道命此变条只没结解问意建月公无系军很情者最立代想已通并提直题党程展五果料象员革位入常文总次品式活设及管特件长求老头基资边流路级少图山统接知较将组见计别她手角期根论运农指几九区强放决西被干做必战先回则任取据处理府研";

/// A legacy-encoding CSV: about 20 MB of Chinese text in GBK, so the
/// reader detects the encoding and decodes every byte.
pub fn csv_gbk(w: &mut dyn Write) -> io::Result<()> {
    const TARGET: usize = 20_000_000;
    let chars: Vec<char> = HANZI.chars().collect();
    let mut rng = Rng::new(SEED ^ 3);
    let mut written = 0;
    let mut row = String::new();
    while written < TARGET {
        row.clear();
        for col in 0..6 {
            if col > 0 {
                row.push(',');
            }
            if col == 3 {
                row.push_str(&rng.below(1_000_000).to_string());
            } else {
                for _ in 0..rng.range(2, 8) {
                    row.push(chars[rng.below(chars.len() as u64) as usize]);
                }
            }
        }
        row.push_str("\r\n");
        let (bytes, _, unmappable) = carrysheet_csv::GBK.encode(&row);
        if unmappable {
            return Err(io::Error::other("a generator character is not in GBK"));
        }
        w.write_all(&bytes)?;
        written += bytes.len();
    }
    Ok(())
}

// ---- BIFF8 (.xls) ----

/// A BIFF stream under construction.
#[derive(Default)]
struct Stream {
    bytes: Vec<u8>,
}

impl Stream {
    fn rec(&mut self, typ: u16, body: &[u8]) {
        self.bytes.extend_from_slice(&typ.to_le_bytes());
        self.bytes
            .extend_from_slice(&(body.len() as u16).to_le_bytes());
        self.bytes.extend_from_slice(body);
    }
}

fn bof8(dt: u16) -> Vec<u8> {
    [
        &0x0600u16.to_le_bytes()[..],
        &dt.to_le_bytes(),
        &0x0DBBu16.to_le_bytes(),
        &0x07CCu16.to_le_bytes(),
        &[0u8; 8],
    ]
    .concat()
}

/// Rows and columns of the generated `.xls` sheet: 65,536 rows (the BIFF8
/// limit) by 16 columns, 1,048,576 cells.
pub const XLS_ROWS: u32 = 65_536;
pub const XLS_COLS: u16 = 16;
const XLS_STRINGS: u32 = 2_000;

/// The globals of a one-sheet BIFF8 workbook, with a placeholder sheet
/// position. Returns the stream and where the BOUNDSHEET position is.
fn xls_globals(strings: u32) -> (Stream, usize) {
    let mut s = Stream::default();
    s.rec(0x0809, &bof8(0x0005)); // BOF, workbook globals
    s.rec(0x0042, &1200u16.to_le_bytes()); // CODEPAGE: UTF-16
    s.rec(0x00E0, &[0u8; 20]); // one XF
    let bound_at = s.bytes.len() + 4;
    // BOUNDSHEET: position (patched), visible worksheet, "Sheet1" compressed.
    let mut bs = vec![0u8; 4];
    bs.extend_from_slice(&[0, 0, 6, 0]);
    bs.extend_from_slice(b"Sheet1");
    s.rec(0x0085, &bs);
    if strings > 0 {
        // SST, split into CONTINUE records at string boundaries.
        let mut pieces: Vec<Vec<u8>> =
            vec![[strings.to_le_bytes(), strings.to_le_bytes()].concat()];
        for i in 0..strings {
            let text = format!("item {i:05} of the list");
            let mut entry = (text.len() as u16).to_le_bytes().to_vec();
            entry.push(0); // compressed (Latin-1) characters
            entry.extend_from_slice(text.as_bytes());
            if pieces.last().map_or(0, Vec::len) + entry.len() > 8224 {
                pieces.push(Vec::new());
            }
            pieces
                .last_mut()
                .expect("one piece")
                .extend_from_slice(&entry);
        }
        for (i, piece) in pieces.iter().enumerate() {
            s.rec(if i == 0 { 0x00FC } else { 0x003C }, piece);
        }
    }
    s.rec(0x000A, &[]); // EOF
    (s, bound_at)
}

fn patch_sheet_position(s: &mut Stream, bound_at: usize) {
    let at = s.bytes.len() as u32;
    s.bytes[bound_at..bound_at + 4].copy_from_slice(&at.to_le_bytes());
}

/// The `Workbook` stream of a 1,048,576-cell sheet: per row, four RK
/// numbers, four NUMBER floats, four shared strings (LABELSST) and four
/// formulas (`A1+E1` style, 262,144 formulas in all). About 26 MB.
pub fn xls_stream() -> Vec<u8> {
    xls_sheet(XLS_ROWS)
}

fn xls_sheet(rows: u32) -> Vec<u8> {
    let mut rng = Rng::new(SEED ^ 4);
    let (mut s, bound_at) = xls_globals(XLS_STRINGS);
    patch_sheet_position(&mut s, bound_at);
    s.rec(0x0809, &bof8(0x0010)); // BOF, worksheet
    let dims = [
        &0u32.to_le_bytes()[..],
        &rows.to_le_bytes(),
        &0u16.to_le_bytes(),
        &XLS_COLS.to_le_bytes(),
        &0u16.to_le_bytes(),
    ]
    .concat();
    s.rec(0x0200, &dims); // DIMENSIONS
    let mut body = Vec::with_capacity(32);
    for row in 0..rows {
        let r = (row as u16).to_le_bytes();
        for col in 0..XLS_COLS {
            body.clear();
            body.extend_from_slice(&r);
            body.extend_from_slice(&col.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes()); // XF
            match col {
                0..=3 => {
                    // RK: an integer, shifted left 2 with the integer flag.
                    let v = rng.below(1 << 20) as u32;
                    body.extend_from_slice(&((v << 2) | 2).to_le_bytes());
                    s.rec(0x027E, &body);
                }
                4..=7 => {
                    let v = rng.below(10_000_000) as f64 / 100.0;
                    body.extend_from_slice(&v.to_le_bytes());
                    s.rec(0x0203, &body);
                }
                8..=11 => {
                    let i = rng.below(u64::from(XLS_STRINGS)) as u32;
                    body.extend_from_slice(&i.to_le_bytes());
                    s.rec(0x00FD, &body);
                }
                _ => {
                    // FORMULA: cached result, flags, chn, then
                    // ptgRef(A{row}) ptgRef(E{row}) ptgAdd, relative refs.
                    body.extend_from_slice(&0f64.to_le_bytes());
                    body.extend_from_slice(&0u16.to_le_bytes());
                    body.extend_from_slice(&0u32.to_le_bytes());
                    body.extend_from_slice(&11u16.to_le_bytes());
                    for ref_col in [col - 12, col - 8] {
                        body.push(0x24);
                        body.extend_from_slice(&r);
                        body.extend_from_slice(&(ref_col | 0xC000).to_le_bytes());
                    }
                    body.push(0x03);
                    s.rec(0x0006, &body);
                }
            }
        }
    }
    s.rec(0x000A, &[]);
    s.bytes
}

/// Turns a compound-file error into an I/O error.
fn cfb_err(e: carrysheet_vba::cfb::CfbError) -> io::Error {
    io::Error::other(e.to_string())
}

/// Wraps a workbook stream in a compound file, as Excel saves `.xls`.
fn compound(workbook: &[u8]) -> io::Result<Vec<u8>> {
    use carrysheet_vba::cfb::{Builder, Version};
    // New entries carry no times, so the generated bytes repeat.
    let mut cf = Builder::new(Version::V3);
    cf.create_stream("/Workbook", workbook.to_vec())
        .map_err(cfb_err)?;
    cf.to_bytes().map_err(cfb_err)
}

/// The 1,048,576-cell `.xls` file.
pub fn xls_1m_cells(w: &mut dyn Write) -> io::Result<()> {
    w.write_all(&compound(&xls_stream())?)
}

/// Hostile: a sheet of 3,000,000 empty records of a type no reader knows,
/// so the per-record work is all there is. 12 MB.
pub fn xls_many_records(w: &mut dyn Write) -> io::Result<()> {
    let (mut s, bound_at) = xls_globals(0);
    patch_sheet_position(&mut s, bound_at);
    s.rec(0x0809, &bof8(0x0010));
    for _ in 0..3_000_000 {
        s.rec(0x7FFF, &[]);
    }
    s.rec(0x000A, &[]);
    w.write_all(&compound(&s.bytes)?)
}

/// The 1,048,576-cell workbook stream encrypted with RC4 CryptoAPI under
/// the password [`XLS_PASSWORD`]: the raw stream, as `decrypt_biff` takes it.
/// Encryption draws a fresh salt, so the bytes differ on every generation.
pub fn xls_rc4_stream(w: &mut dyn Write) -> io::Result<()> {
    let password = carrysheet_crypto::Password::new(XLS_PASSWORD);
    let enc = carrysheet_crypto::encrypt_biff(&xls_stream(), &password)
        .map_err(|e| io::Error::other(e.to_string()))?;
    w.write_all(&enc)
}

/// The password of [`xls_rc4_stream`].
pub const XLS_PASSWORD: &str = "bench";

// ---- VBA (MS-OVBA) ----

/// A container whose chunks each expand 6 bytes to 4096: a decompression
/// bomb of `chunks` * 4096 bytes (from the vba crate's hostile tests).
pub fn ovba_bomb(chunks: usize) -> Vec<u8> {
    let mut out = vec![0x01];
    for _ in 0..chunks {
        out.extend_from_slice(&(0xB000u16 | 3).to_le_bytes());
        out.push(0b10);
        out.push(b'a');
        out.extend_from_slice(&4092u16.to_le_bytes());
    }
    out
}

/// A valid MS-OVBA container that stores `data` as literals only.
fn ovba_literal(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x01];
    for piece in data.chunks(3584) {
        let mut body = Vec::new();
        for group in piece.chunks(8) {
            body.push(0x00);
            body.extend_from_slice(group);
        }
        let header = 0xB000 | (body.len() as u16 + 2 - 3);
        out.extend_from_slice(&header.to_le_bytes());
        out.extend_from_slice(&body);
    }
    out
}

fn dir_rec(out: &mut Vec<u8>, id: u16, data: &[u8]) {
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// The `dir` stream of a project with one standard module, `Module1`.
fn vba_dir(pcode: u32) -> Vec<u8> {
    let mut d = Vec::new();
    dir_rec(&mut d, 0x0001, &1u32.to_le_bytes()); // SYSKIND
    dir_rec(&mut d, 0x0002, &0x0409u32.to_le_bytes()); // LCID
    dir_rec(&mut d, 0x0014, &0x0409u32.to_le_bytes()); // LCIDINVOKE
    dir_rec(&mut d, 0x0003, &1252u16.to_le_bytes()); // CODEPAGE
    dir_rec(&mut d, 0x0004, b"VBAProject"); // NAME
    dir_rec(&mut d, 0x0005, b""); // DOCSTRING
    dir_rec(&mut d, 0x0040, b"");
    dir_rec(&mut d, 0x0006, b""); // HELPFILEPATH
    dir_rec(&mut d, 0x003D, b"");
    dir_rec(&mut d, 0x0007, &0u32.to_le_bytes()); // HELPCONTEXT
    dir_rec(&mut d, 0x0008, &0u32.to_le_bytes()); // LIBFLAGS
    d.extend_from_slice(&0x0009u16.to_le_bytes()); // VERSION
    d.extend_from_slice(&4u32.to_le_bytes());
    d.extend_from_slice(&1u32.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes());
    dir_rec(&mut d, 0x000C, b""); // CONSTANTS
    dir_rec(&mut d, 0x003C, b"");
    dir_rec(&mut d, 0x000F, &1u16.to_le_bytes()); // one module
    dir_rec(&mut d, 0x0013, &0xFFFFu16.to_le_bytes());
    dir_rec(&mut d, 0x0019, b"Module1");
    dir_rec(&mut d, 0x0047, &utf16("Module1"));
    dir_rec(&mut d, 0x001A, b"Module1");
    dir_rec(&mut d, 0x0032, &utf16("Module1"));
    dir_rec(&mut d, 0x001C, b"");
    dir_rec(&mut d, 0x0048, b"");
    dir_rec(&mut d, 0x0031, &pcode.to_le_bytes()); // MODULEOFFSET
    dir_rec(&mut d, 0x001E, &0u32.to_le_bytes());
    dir_rec(&mut d, 0x002C, &0xFFFFu16.to_le_bytes());
    d.extend_from_slice(&0x0021u16.to_le_bytes()); // procedural module
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&0x002Bu16.to_le_bytes()); // module terminator
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&0x0010u16.to_le_bytes()); // dir terminator
    d.extend_from_slice(&0u32.to_le_bytes());
    d
}

/// Hostile (issue 8, from the fuzzing review): a `vbaProject.bin` whose one
/// module hides a 24 KB source that expands to 16 MiB, inside the default
/// limits.
pub fn vba_bomb_module(w: &mut dyn Write) -> io::Result<()> {
    use carrysheet_vba::cfb::{Builder, Version};
    const PCODE: u32 = 700;
    let project = "ID=\"{00000000-0000-0000-0000-000000000000}\"\r\nModule=Module1\r\nName=\"VBAProject\"\r\n";
    let mut module: Vec<u8> = (0..PCODE).map(|i| (i * 7 % 251) as u8).collect();
    module.extend(ovba_bomb(4095));
    let streams: [(&str, Vec<u8>); 4] = [
        ("/PROJECT", project.as_bytes().to_vec()),
        ("/VBA/dir", ovba_literal(&vba_dir(PCODE))),
        (
            "/VBA/_VBA_PROJECT",
            vec![0xCC, 0x61, 0xFF, 0xFF, 0x00, 0x00, 0x00],
        ),
        ("/VBA/Module1", module),
    ];
    let mut cf = Builder::new(Version::V3);
    cf.create_storage("/VBA").map_err(cfb_err)?;
    for (path, bytes) in streams {
        cf.create_stream(path, bytes).map_err(cfb_err)?;
    }
    w.write_all(&cf.to_bytes().map_err(cfb_err)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_repeats() {
        let a: Vec<u64> = (0..4).scan(Rng::new(7), |r, _| Some(r.next())).collect();
        let b: Vec<u64> = (0..4).scan(Rng::new(7), |r, _| Some(r.next())).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn small_generators_are_deterministic() {
        for make in [
            csv_empty_cells as fn(&mut dyn Write) -> io::Result<()>,
            vba_bomb_module,
        ] {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            make(&mut a).unwrap();
            make(&mut b).unwrap();
            assert_eq!(a, b);
        }
        assert_eq!(compound(b"x").unwrap(), compound(b"x").unwrap());
    }

    #[test]
    fn generated_xls_reads_back() {
        // A cut-down sheet through the same record writers.
        let stream = xls_sheet(100);
        let book = carrysheet_biff::read(&compound(&stream).unwrap()).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.cells.len(), 100 * XLS_COLS as usize);
        let f = sheet.cell(0, 12).unwrap().formula.as_ref().unwrap();
        assert_eq!(f.text.as_str(), Some("A1+E1"));
    }

    #[test]
    fn vba_bomb_reads() {
        let mut bin = Vec::new();
        vba_bomb_module(&mut bin).unwrap();
        let project = carrysheet_vba::read_vba_project_bin(&bin).unwrap();
        assert_eq!(project.modules.len(), 1);
    }
}
