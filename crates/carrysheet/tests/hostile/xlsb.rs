//! Small XLSB packages built in code, from Microsoft's [MS-XLSB]: a
//! workbook part, one worksheet part and an optional shared-string part.

use crate::common::{entry, zip};

/// One record: the type and the size as 7-bit groups, then the payload.
pub fn rec(out: &mut Vec<u8>, id: u16, payload: &[u8]) {
    if id < 0x80 {
        out.push(id as u8);
    } else {
        out.push((id & 0x7F) as u8 | 0x80);
        out.push((id >> 7) as u8);
    }
    let mut size = payload.len();
    loop {
        let byte = (size & 0x7F) as u8;
        size >>= 7;
        if size == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out.extend_from_slice(payload);
}

/// An XLWideString: a character count, then UTF-16LE.
pub fn wide(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = (units.len() as u32).to_le_bytes().to_vec();
    out.extend(units.iter().flat_map(|u| u.to_le_bytes()));
    out
}

/// The workbook part: one visible sheet, "S", at relationship `rId1`.
pub fn workbook_part() -> Vec<u8> {
    let mut out = Vec::new();
    rec(&mut out, 131, &[]);
    rec(&mut out, 143, &[]);
    let mut sheet = vec![0, 0, 0, 0, 1, 0, 0, 0];
    sheet.extend(wide("rId1"));
    sheet.extend(wide("S"));
    rec(&mut out, 156, &sheet);
    rec(&mut out, 144, &[]);
    rec(&mut out, 132, &[]);
    out
}

/// A row header for row `row`, with no column spans.
pub fn row_header(row: u32) -> Vec<u8> {
    let mut out = row.to_le_bytes().to_vec();
    out.extend([0u8; 9]);
    out.extend(0u32.to_le_bytes());
    out
}

/// A number cell (BrtCellReal) at `col`.
pub fn number_cell(col: u32, value: f64) -> Vec<u8> {
    let mut out = col.to_le_bytes().to_vec();
    out.extend(0u32.to_le_bytes());
    out.extend(value.to_le_bytes());
    out
}

/// A worksheet part around `data`, the records inside BrtBeginSheetData.
pub fn sheet_part(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    rec(&mut out, 129, &[]);
    rec(&mut out, 145, &[]);
    out.extend_from_slice(data);
    rec(&mut out, 146, &[]);
    rec(&mut out, 130, &[]);
    out
}

/// A small sheet: A1 = 1.5, B1 = the shared string 0, A2 = "inline".
pub fn small_sheet() -> Vec<u8> {
    let mut data = Vec::new();
    rec(&mut data, 0, &row_header(0));
    rec(&mut data, 5, &number_cell(0, 1.5));
    let mut isst = 1u32.to_le_bytes().to_vec();
    isst.extend(0u32.to_le_bytes());
    isst.extend(0u32.to_le_bytes());
    rec(&mut data, 7, &isst);
    rec(&mut data, 0, &row_header(1));
    let mut text = 0u32.to_le_bytes().to_vec();
    text.extend(0u32.to_le_bytes());
    text.extend(wide("inline"));
    rec(&mut data, 6, &text);
    sheet_part(&data)
}

/// A shared-string part of `strings`.
pub fn strings_part(strings: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    let count = (strings.len() as u32).to_le_bytes();
    rec(&mut out, 159, &[count, count].concat());
    for s in strings {
        let mut item = vec![0u8];
        item.extend(wide(s));
        rec(&mut out, 19, &item);
    }
    rec(&mut out, 160, &[]);
    out
}

/// An XLSB package of the given parts; `strings` adds a shared-string part.
pub fn package(workbook: &[u8], sheet: &[u8], strings: Option<&[u8]>) -> Vec<u8> {
    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
    let mut types = String::from(
        r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/xl/workbook.bin" ContentType="application/vnd.ms-excel.sheet.binary.macroEnabled.main"/><Override PartName="/xl/worksheets/sheet1.bin" ContentType="application/vnd.ms-excel.worksheet"/>"#,
    );
    if strings.is_some() {
        types.push_str(r#"<Override PartName="/xl/sharedStrings.bin" ContentType="application/vnd.ms-excel.sharedStrings"/>"#);
    }
    types.push_str("</Types>");
    let rels = format!(
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}officeDocument" Target="xl/workbook.bin"/></Relationships>"#
    );
    let mut book_rels = format!(
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}worksheet" Target="worksheets/sheet1.bin"/>"#
    );
    if strings.is_some() {
        book_rels.push_str(&format!(
            r#"<Relationship Id="rId2" Type="{REL}sharedStrings" Target="sharedStrings.bin"/>"#
        ));
    }
    book_rels.push_str("</Relationships>");
    let mut entries = vec![
        entry("[Content_Types].xml", types),
        entry("_rels/.rels", rels),
        entry("xl/workbook.bin", workbook),
        entry("xl/_rels/workbook.bin.rels", book_rels),
        entry("xl/worksheets/sheet1.bin", sheet),
    ];
    if let Some(s) = strings {
        entries.push(entry("xl/sharedStrings.bin", s));
    }
    zip(&entries)
}

/// A small, valid XLSB workbook.
pub fn small_book() -> Vec<u8> {
    package(
        &workbook_part(),
        &small_sheet(),
        Some(&strings_part(&["shared"])),
    )
}
