//! Finds the BIFF stream: inside an OLE2 compound file, or the file itself.

use carrysheet_cfb::{CfbError, CompoundFile};

use crate::error::{BiffError, BiffErrorKind};
use crate::model::Container;

/// BOF record types that can start a raw BIFF stream.
const BOF_TYPES: [u16; 4] = [0x0009, 0x0209, 0x0409, 0x0809];

pub(crate) fn is_ole2(bytes: &[u8]) -> bool {
    carrysheet_cfb::is_compound_file(bytes)
}

pub(crate) fn starts_with_bof(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && BOF_TYPES.contains(&u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Returns the workbook stream and where it came from.
pub(crate) fn workbook_stream(
    bytes: &[u8],
    max_stream: usize,
) -> Result<(std::borrow::Cow<'_, [u8]>, Container), BiffError> {
    if is_ole2(bytes) {
        return ole2_stream(bytes, max_stream).map(|(s, c)| (std::borrow::Cow::Owned(s), c));
    }
    if starts_with_bof(bytes) {
        if bytes.len() > max_stream {
            return Err(BiffError::limit("stream size in bytes", max_stream, None));
        }
        return Ok((std::borrow::Cow::Borrowed(bytes), Container::Raw));
    }
    let detail = if bytes.len() >= 4 && bytes[..4] == [0x50, 0x4B, 0x03, 0x04] {
        "it is a ZIP package, which is how .xlsx, .xlsm and .ods files are stored".to_string()
    } else if bytes.is_empty() {
        "the file is empty".to_string()
    } else {
        "it starts with neither the OLE2 signature nor a BIFF BOF record".to_string()
    };
    Err(BiffError::new(BiffErrorKind::NotXls { detail }, Some(0)))
}

fn ole2_stream(bytes: &[u8], max_stream: usize) -> Result<(Vec<u8>, Container), BiffError> {
    let container_err = |e: CfbError| {
        BiffError::new(
            BiffErrorKind::Container {
                detail: e.to_string(),
            },
            None,
        )
    };
    let cf = CompoundFile::open(bytes).map_err(container_err)?;
    let mut streams = Vec::new();
    let mut found = None;
    for entry in cf.children(cf.root()) {
        if !entry.is_stream() {
            continue;
        }
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        // Prefer the BIFF8 "Workbook" stream when a file has both.
        if lower == "workbook" || (lower == "book" && found.is_none()) {
            found = Some((name.clone(), entry));
        }
        if streams.len() < 16 {
            streams.push(name);
        }
    }
    let Some((name, entry)) = found else {
        return Err(BiffError::new(
            BiffErrorKind::NoWorkbookStream { streams },
            None,
        ));
    };
    if entry.size() > max_stream as u64 {
        return Err(BiffError::limit("stream size in bytes", max_stream, None));
    }
    let data = cf.read(entry).map_err(container_err)?;
    Ok((data, Container::Ole2 { stream: name }))
}
