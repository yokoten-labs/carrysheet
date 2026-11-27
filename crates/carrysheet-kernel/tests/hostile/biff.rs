//! BIFF streams built in code, from the removed BIFF reader's tests: the
//! sample builders (BIFF2, BIFF4, BIFF5 and BIFF8 with a split SST) and
//! the helpers of its hostile tests. [MS-XLS] names the records.

/// A BIFF stream under construction.
#[derive(Default)]
pub struct Stream {
    pub bytes: Vec<u8>,
}

impl Stream {
    pub fn rec(&mut self, typ: u16, body: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(&typ.to_le_bytes());
        self.bytes
            .extend_from_slice(&(body.len() as u16).to_le_bytes());
        self.bytes.extend_from_slice(body);
        self
    }

    pub fn pos(&self) -> usize {
        self.bytes.len()
    }
}

/// Concatenates byte slices.
pub fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

pub fn u16le(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

pub fn u32le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

pub fn f64le(v: f64) -> [u8; 8] {
    v.to_le_bytes()
}

/// A BIFF2 worksheet: formats, every BIFF2 cell record, a formula with a
/// string result, IXFE, COLWIDTH, a defined name and the 1904 date system.
pub fn biff2_sheet() -> Vec<u8> {
    let mut s = Stream::default();
    s.rec(0x0009, &cat(&[&u16le(0x0002), &u16le(0x0010)]));
    s.rec(0x0042, &u16le(32769)); // CODEPAGE: windows-1252 (BIFF2 form)
    s.rec(0x0022, &u16le(1)); // DATEMODE: 1904
    s.rec(0x001E, b"\x07General");
    s.rec(0x001E, b"\x040.00");
    s.rec(0x001E, b"\x06m/d/yy");
    s.rec(0x0000, &cat(&[&u16le(0), &u16le(4), &u16le(0), &u16le(3)])); // DIMENSIONS
    s.rec(0x0024, &cat(&[&[0, 1], &u16le(0x0A00)])); // COLWIDTH A:B
    // INTEGER A1 = 7, format 1 ("0.00").
    s.rec(0x0002, &cat(&[&u16le(0), &u16le(0), &[0, 1, 0], &u16le(7)]));
    // NUMBER B1 = 35000, format 2 (a date).
    s.rec(
        0x0003,
        &cat(&[&u16le(0), &u16le(1), &[0, 2, 0], &f64le(35000.0)]),
    );
    // LABEL C1 = "café" in windows-1252.
    s.rec(
        0x0004,
        &cat(&[&u16le(0), &u16le(2), &[0, 0, 0], &[4], b"caf\xE9"]),
    );
    // BOOLERR A2 = TRUE, B2 = #DIV/0!.
    s.rec(0x0005, &cat(&[&u16le(1), &u16le(0), &[0, 0, 0], &[1, 0]]));
    s.rec(
        0x0005,
        &cat(&[&u16le(1), &u16le(1), &[0, 0, 0], &[0x07, 1]]),
    );
    // IXFE then BLANK C2 with XF 63 -> 21.
    s.rec(0x0044, &u16le(21));
    s.rec(0x0001, &cat(&[&u16le(1), &u16le(2), &[63, 0, 0]]));
    // FORMULA A3 = A1+1 (cached 8).
    let rgce = [0x44, 0x00, 0xC0, 0x00, 0x1E, 1, 0, 0x03];
    s.rec(
        0x0006,
        &cat(&[
            &u16le(2),
            &u16le(0),
            &[0, 0, 0],
            &f64le(8.0),
            &[0],
            &[rgce.len() as u8],
            &rgce,
        ]),
    );
    // FORMULA B3 = "x"&"y" with a string result in a STRING record.
    let rgce = [0x17, 1, b'x', 0x17, 1, b'y', 0x08];
    s.rec(
        0x0006,
        &cat(&[
            &u16le(2),
            &u16le(1),
            &[0, 0, 0],
            &[0, 0, 0, 0, 0, 0, 0xFF, 0xFF],
            &[0],
            &[rgce.len() as u8],
            &rgce,
        ]),
    );
    s.rec(0x0007, b"\x02xy");
    // NAME "Total" = $A$1:$B$2 (BIFF2: 2 flag bytes, key, cch, cce u8).
    let rgce = [0x25, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01];
    s.rec(
        0x0018,
        &cat(&[&[0, 0, 0], &[5], &[rgce.len() as u8], b"Total", &rgce]),
    );
    s.rec(0x000A, &[]);
    s.bytes
}

/// A BIFF5 workbook stream with one sheet whose name and text are in
/// `codepage`, encoded with `enc`.
pub fn biff5_book(codepage: u16, sheet_name: &[u8], text: &[u8]) -> Vec<u8> {
    let mut s = Stream::default();
    s.rec(
        0x0809,
        &cat(&[
            &u16le(0x0500),
            &u16le(0x0005),
            &u16le(0x0DBB),
            &u16le(0x07CC),
        ]),
    );
    s.rec(0x0042, &u16le(codepage));
    s.rec(0x041E, &cat(&[&u16le(0xA4), &[4], b"0.0%"]));
    // XF 0 General, XF 1 with format 0xA4.
    s.rec(0x00E0, &cat(&[&u16le(0), &u16le(0), &[0u8; 12]]));
    s.rec(0x00E0, &cat(&[&u16le(0), &u16le(0xA4), &[0u8; 12]]));
    let bound_at = s.pos();
    s.rec(
        0x0085,
        &cat(&[&u32le(0), &[0, 0], &[sheet_name.len() as u8], sheet_name]),
    );
    // EXTERNSHEET pointing at our own sheet, for 3D references.
    s.rec(
        0x0017,
        &cat(&[&[1 + sheet_name.len() as u8, 0x03], sheet_name]),
    );
    // NAME "Rate" = 3D reference to the sheet's $B$1.
    let mut rgce = vec![0x3A];
    rgce.extend_from_slice(&(-1i16).to_le_bytes());
    rgce.extend_from_slice(&[0u8; 8]);
    rgce.extend_from_slice(&0i16.to_le_bytes());
    rgce.extend_from_slice(&0i16.to_le_bytes());
    rgce.extend_from_slice(&[0x00, 0x00, 0x01]);
    s.rec(
        0x0018,
        &cat(&[
            &u16le(0),
            &[0, 4],
            &u16le(rgce.len() as u16),
            &u16le(0),
            &u16le(0),
            &[0, 0, 0, 0],
            b"Rate",
            &rgce,
        ]),
    );
    s.rec(0x000A, &[]);
    let sheet_at = s.pos() as u32;
    s.bytes[bound_at + 4..bound_at + 8].copy_from_slice(&sheet_at.to_le_bytes());
    s.rec(
        0x0809,
        &cat(&[
            &u16le(0x0500),
            &u16le(0x0010),
            &u16le(0x0DBB),
            &u16le(0x07CC),
        ]),
    );
    s.rec(
        0x0200,
        &cat(&[&u16le(0), &u16le(2), &u16le(0), &u16le(3), &u16le(0)]),
    );
    // LABEL A1 with the text.
    s.rec(
        0x0204,
        &cat(&[
            &u16le(0),
            &u16le(0),
            &u16le(0),
            &u16le(text.len() as u16),
            text,
        ]),
    );
    // NUMBER B1 = 0.25 with XF 1.
    s.rec(
        0x0203,
        &cat(&[&u16le(0), &u16le(1), &u16le(1), &f64le(0.25)]),
    );
    // FORMULA C1 = Rate*2 (PtgName, BIFF5 layout: index + 12 bytes).
    let mut rgce = vec![0x43];
    rgce.extend_from_slice(&u16le(1));
    rgce.extend_from_slice(&[0u8; 12]);
    rgce.extend_from_slice(&[0x1E, 2, 0, 0x05]);
    s.rec(
        0x0006,
        &cat(&[
            &u16le(0),
            &u16le(2),
            &u16le(0),
            &f64le(0.5),
            &u16le(0),
            &u32le(0),
            &u16le(rgce.len() as u16),
            &rgce,
        ]),
    );
    // FORMULA C2 = SUM(A2:B2)&"!" with a cached string from a STRING record.
    let rgce = [
        0x25, 1, 0xC0, 1, 0xC0, 0, 1, 0x19, 0x10, 0, 0, 0x17, 1, b'!', 0x08,
    ];
    s.rec(
        0x0006,
        &cat(&[
            &u16le(1),
            &u16le(2),
            &u16le(0),
            &[0, 0, 0, 0, 0, 0, 0xFF, 0xFF],
            &u16le(0),
            &u32le(0),
            &u16le(rgce.len() as u16),
            &rgce,
        ]),
    );
    s.rec(0x0207, &cat(&[&u16le(2), b"0!"]));
    s.rec(0x000A, &[]);
    s.bytes
}

/// A BIFF8 workbook stream whose SST is split across CONTINUE records in
/// the middle of strings, switching between compressed and UTF-16 text,
/// and through the formatting runs of a rich string.
pub fn biff8_split_sst() -> Vec<u8> {
    let mut s = Stream::default();
    let bof8 = |dt: u16| {
        cat(&[
            &u16le(0x0600),
            &u16le(dt),
            &u16le(0x0DBB),
            &u16le(0x07CC),
            &[0u8; 8],
        ])
    };
    s.rec(0x0809, &bof8(0x0005));
    s.rec(0x0042, &u16le(1200));
    s.rec(0x00E0, &cat(&[&u16le(0), &u16le(0), &[0u8; 16]]));
    let bound_at = s.pos();
    // BOUNDSHEET with a UTF-16 name "表1".
    s.rec(
        0x0085,
        &cat(&[&u32le(0), &[0, 0], &[2, 1], &u16le(0x8868), &u16le(0x0031)]),
    );
    // SST: 3 strings.
    //  0: "ABC" compressed, then CONTINUE flips to UTF-16 for "中文".
    //  1: rich "Hi" with 2 runs; the CONTINUE falls inside the runs.
    //  2: "xyz" compressed, header in one record and characters in the next.
    let mut sst = cat(&[&u32le(3), &u32le(3)]);
    sst.extend_from_slice(&cat(&[&u16le(5), &[0], b"ABC"]));
    let mut c1 = vec![0x01];
    c1.extend_from_slice(&u16le(0x4E2D));
    c1.extend_from_slice(&u16le(0x6587));
    c1.extend_from_slice(&cat(&[
        &u16le(2),
        &[0x08],
        &u16le(2),
        b"Hi",
        &u16le(0),
        &u16le(1),
    ]));
    let c2 = cat(&[&u16le(1), &u16le(2), &u16le(3), &[0]]);
    let c3 = cat(&[&[0], b"xyz"]);
    s.rec(0x00FC, &sst);
    s.rec(0x003C, &c1);
    s.rec(0x003C, &c2);
    s.rec(0x003C, &c3);
    s.rec(0x000A, &[]);
    let sheet_at = s.pos() as u32;
    s.bytes[bound_at + 4..bound_at + 8].copy_from_slice(&sheet_at.to_le_bytes());
    s.rec(0x0809, &bof8(0x0010));
    for (col, isst) in [(0u16, 0u32), (1, 1), (2, 2)] {
        s.rec(
            0x00FD,
            &cat(&[&u16le(0), &u16le(col), &u16le(0), &u32le(isst)]),
        );
    }
    // MULRK A2:C2 = 1, -2, 0.5 (as /100 integer 50).
    s.rec(
        0x00BD,
        &cat(&[
            &u16le(1),
            &u16le(0),
            &u16le(0),
            &u32le((1 << 2) | 2),
            &u16le(0),
            &u32le(((-2i32) << 2) as u32 | 2),
            &u16le(0),
            &u32le((50 << 2) | 3),
            &u16le(2),
        ]),
    );
    // MULBLANK A3:B3.
    s.rec(
        0x00BE,
        &cat(&[&u16le(2), &u16le(0), &u16le(0), &u16le(0), &u16le(1)]),
    );
    // MERGEDCELLS A4:B5.
    s.rec(
        0x00E5,
        &cat(&[&u16le(1), &u16le(3), &u16le(4), &u16le(0), &u16le(1)]),
    );
    s.rec(0x000A, &[]);
    s.bytes
}
/// A BIFF4W workbook (Excel 4.0 bound workbook): a globals substream with
/// BUNDLESHEET records, then one worksheet substream per sheet.
pub fn biff4_workbook() -> Vec<u8> {
    let mut s = Stream::default();
    let bof = |dt: u16| cat(&[&u16le(0x0000), &u16le(dt), &u16le(0x0DBB)]);
    s.rec(0x0409, &bof(0x0100));
    s.rec(0x0042, &u16le(1252));
    let names: [&[u8]; 2] = [b"Sales", b"Costs \xA3"];
    let mut bundles = Vec::new();
    for name in names {
        bundles.push(s.pos());
        s.rec(
            0x008F,
            &cat(&[&u32le(0), &u16le(0), &[name.len() as u8], name]),
        );
    }
    s.rec(0x000A, &[]);
    for (i, at) in bundles.into_iter().enumerate() {
        let start = s.pos() as u32;
        s.bytes[at + 4..at + 8].copy_from_slice(&start.to_le_bytes());
        s.rec(0x0409, &bof(0x0010));
        // NUMBER (BIFF3/4 layout): row, col, xf, value.
        s.rec(
            0x0203,
            &cat(&[
                &u16le(0),
                &u16le(0),
                &u16le(0),
                &f64le(10.0 * (i + 1) as f64),
            ]),
        );
        // LABEL (BIFF3/4): row, col, xf, cch u16, bytes.
        s.rec(
            0x0204,
            &cat(&[&u16le(1), &u16le(0), &u16le(0), &u16le(3), b"\xA3 5"]),
        );
        s.rec(0x000A, &[]);
    }
    s.bytes
}

pub fn bof8(s: &mut Stream, dt: u16) {
    s.rec(
        0x0809,
        &cat(&[
            &u16le(0x0600),
            &u16le(dt),
            &u16le(0),
            &u16le(1997),
            &u32le(0),
            &u32le(6),
        ]),
    );
}

pub fn eof(s: &mut Stream) {
    s.rec(0x000A, &[]);
}

/// A BOUNDSHEET with a placeholder offset; returns where to patch it.
pub fn boundsheet(s: &mut Stream, name: &str) -> usize {
    let at = s.pos();
    s.rec(
        0x0085,
        &cat(&[&u32le(0), &[0, 0, name.len() as u8, 0], name.as_bytes()]),
    );
    at
}

pub fn patch(s: &mut Stream, bound_at: usize, offset: usize) {
    s.bytes[bound_at + 4..bound_at + 8].copy_from_slice(&(offset as u32).to_le_bytes());
}

/// A BIFF8 FORMULA record.
pub fn formula(s: &mut Stream, row: u16, col: u16, tokens: &[u8], flags: u16) {
    s.rec(
        0x0006,
        &cat(&[
            &u16le(row),
            &u16le(col),
            &u16le(0),
            &[0u8; 8],
            &u16le(flags),
            &u32le(0),
            &u16le(tokens.len() as u16),
            tokens,
        ]),
    );
}

pub fn ptg_exp(row: u16, col: u16) -> Vec<u8> {
    cat(&[&[0x01], &u16le(row), &u16le(col)])
}

/// A SHRFMLA record (BIFF8): RefU, reserved, cUse, cce, tokens.
pub fn shrfmla(s: &mut Stream, r1: u16, r2: u16, c1: u8, c2: u8, tokens: &[u8]) {
    s.rec(
        0x04BC,
        &cat(&[
            &u16le(r1),
            &u16le(r2),
            &[c1, c2, 0, 1],
            &u16le(tokens.len() as u16),
            tokens,
        ]),
    );
}

/// A one-sheet BIFF8 workbook stream whose sheet holds `body`, after
/// `globals`.
pub fn one_sheet(globals: impl FnOnce(&mut Stream), body: impl FnOnce(&mut Stream)) -> Vec<u8> {
    let mut s = Stream::default();
    bof8(&mut s, 0x0005);
    globals(&mut s);
    let b = boundsheet(&mut s, "Sheet1");
    eof(&mut s);
    let start = s.pos();
    patch(&mut s, b, start);
    bof8(&mut s, 0x0010);
    body(&mut s);
    eof(&mut s);
    s.bytes
}

/// The BIFF version of a workbook: the BOF of a raw stream (BIFF2 to
/// BIFF4) or of the `Workbook` stream in a compound file. A compound file
/// with a `Book` stream and no `Workbook` stream is BIFF5 (Excel 5 and 95
/// name it so), whatever its BOF says and even when the stream is cut.
pub fn version(file: &[u8]) -> Option<u16> {
    let stream = if file.starts_with(&crate::cfb::SIGNATURE) {
        if !crate::cfb::has_stream(file, "Workbook") && crate::cfb::has_stream(file, "Book") {
            return Some(5);
        }
        crate::cfb::read_stream(file, "Workbook")?
    } else {
        file.to_vec()
    };
    let id = u16::from_le_bytes(stream.get(..2)?.try_into().ok()?);
    let vers = u16::from_le_bytes(stream.get(4..6)?.try_into().ok()?);
    match id {
        0x0009 => Some(2),
        0x0209 => Some(3),
        0x0409 => Some(4),
        0x0809 if vers == 0x0600 => Some(8),
        0x0809 if vers == 0x0500 => Some(5),
        _ => None,
    }
}
