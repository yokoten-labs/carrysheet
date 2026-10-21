//! A minimal zip reader for the package inside a DataMashup, written from
//! PKWARE's APPNOTE.TXT. It reads the central directory, then stored or
//! deflated entries, inflating each only up to a byte budget. No zip64, no
//! encryption, no multi-disk archives: none of them appear in a DataMashup.
//!
//! The directory is checked before anything is read: no two entries may
//! share a name or overlap in the file, so no bytes can be read twice under
//! different directory entries, and a stored entry's two sizes must agree.

use std::collections::HashSet;

use flate2::{Decompress, FlushDecompress, Status};

use crate::mask::clip;

const END_OF_DIRECTORY: u32 = 0x0605_4b50;
const DIRECTORY_ENTRY: u32 = 0x0201_4b50;
const LOCAL_HEADER: u32 = 0x0403_4b50;

/// One file listed in the central directory.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub name: String,
    /// The name as problems show it: cut to `Limits::max_name_chars`, with
    /// a note of how much was cut.
    pub label: String,
    method: u16,
    encrypted: bool,
    compressed_size: u64,
    pub size: u64,
    local_offset: u64,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at.checked_add(2)?)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at.checked_add(4)?)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn index(value: u64) -> Result<usize, String> {
    usize::try_from(value)
        .map_err(|_| "an offset in the zip is too large for this machine".to_string())
}

/// Lists the entries, refusing more than `max_entries`.
pub(crate) fn entries(
    data: &[u8],
    max_entries: usize,
    max_name_chars: usize,
) -> Result<Vec<Entry>, ReadError> {
    // The end record is 22 bytes plus a comment of up to 65,535 bytes.
    let earliest = data.len().saturating_sub(22 + 0xFFFF);
    let end = (earliest..=data.len().saturating_sub(22))
        .rev()
        .find(|&at| u32_at(data, at) == Some(END_OF_DIRECTORY))
        .ok_or("the zip has no end-of-directory record")?;
    let total = usize::from(u16_at(data, end + 10).ok_or("the zip end record is cut short")?);
    let directory_offset = u32_at(data, end + 16).ok_or("the zip end record is cut short")?;
    if total == 0xFFFF || directory_offset == 0xFFFF_FFFF {
        return Err("the zip uses zip64, which a DataMashup never needs".into());
    }
    if total > max_entries {
        return Err(ReadError::TooMany { count: total });
    }
    let mut at = index(u64::from(directory_offset))?;
    let mut out = Vec::with_capacity(total);
    let mut names = HashSet::with_capacity(total);
    for _ in 0..total {
        if u32_at(data, at) != Some(DIRECTORY_ENTRY) {
            return Err("the zip central directory is damaged".into());
        }
        let short = "a zip directory entry is cut short";
        let flags = u16_at(data, at + 8).ok_or(short)?;
        let method = u16_at(data, at + 10).ok_or(short)?;
        let compressed_size = u32_at(data, at + 20).ok_or(short)?;
        let size = u32_at(data, at + 24).ok_or(short)?;
        let name_len = usize::from(u16_at(data, at + 28).ok_or(short)?);
        let extra_len = usize::from(u16_at(data, at + 30).ok_or(short)?);
        let comment_len = usize::from(u16_at(data, at + 32).ok_or(short)?);
        let local_offset = u32_at(data, at + 42).ok_or(short)?;
        let name = data.get(at + 46..at + 46 + name_len).ok_or(short)?;
        let name = String::from_utf8_lossy(name).into_owned();
        let label = clip(name.clone(), max_name_chars);
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("the zip lists the entry {label} twice").into());
        }
        if method == 0 && compressed_size != size {
            return Err(format!(
                "the stored zip entry {label} gives two different sizes ({compressed_size} and {size} bytes)"
            )
            .into());
        }
        out.push(Entry {
            name,
            label,
            method,
            encrypted: flags & 1 != 0,
            compressed_size: u64::from(compressed_size),
            size: u64::from(size),
            local_offset: u64::from(local_offset),
        });
        at += 46 + name_len + extra_len + comment_len;
    }
    check_no_overlap(&out)?;
    Ok(out)
}

/// Refuses entries whose local header and data (as far as the directory
/// says) overlap another's.
fn check_no_overlap(entries: &[Entry]) -> Result<(), String> {
    let mut spans: Vec<(u64, u64, &str)> = entries
        .iter()
        .map(|e| {
            let header = 30 + e.name.len() as u64;
            (
                e.local_offset,
                e.local_offset + header + e.compressed_size,
                e.label.as_str(),
            )
        })
        .collect();
    spans.sort_unstable();
    for pair in spans.windows(2) {
        let ((_, end, first), (start, _, second)) = (pair[0], pair[1]);
        if start < end {
            return Err(format!(
                "the zip entries {first} and {second} overlap; a zip whose entries share bytes is refused"
            ));
        }
    }
    Ok(())
}

/// Why an entry could not be read.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// It is, or inflates to, more than the bytes allowed; `size` when the
    /// directory says how much.
    TooLarge { size: Option<u64> },
    /// The directory lists more entries than allowed.
    TooMany { count: usize },
    /// It is damaged or uses something this reader does not support.
    Bad(String),
}

impl From<String> for ReadError {
    fn from(why: String) -> Self {
        Self::Bad(why)
    }
}

impl From<&str> for ReadError {
    fn from(why: &str) -> Self {
        Self::Bad(why.to_string())
    }
}

/// Reads one entry, inflating at most `max_bytes`.
pub(crate) fn read(data: &[u8], entry: &Entry, max_bytes: u64) -> Result<Vec<u8>, ReadError> {
    if entry.encrypted {
        return Err(format!("the zip entry {} is encrypted", entry.label).into());
    }
    let stored_size = if entry.method == 0 {
        entry.compressed_size
    } else {
        0
    };
    if entry.size.max(stored_size) > max_bytes {
        return Err(ReadError::TooLarge {
            size: Some(entry.size.max(stored_size)),
        });
    }
    let local = index(entry.local_offset)?;
    if u32_at(data, local) != Some(LOCAL_HEADER) {
        return Err(format!("the zip entry {} has no local header", entry.label).into());
    }
    let short = "a zip local header is cut short";
    let name_len = usize::from(u16_at(data, local + 26).ok_or(short)?);
    let extra_len = usize::from(u16_at(data, local + 28).ok_or(short)?);
    let start = local + 30 + name_len + extra_len;
    let end = start
        .checked_add(index(entry.compressed_size)?)
        .ok_or("a zip entry size overflows")?;
    let compressed = data.get(start..end).ok_or_else(|| {
        format!(
            "the zip entry {} runs past the end of the package",
            entry.label
        )
    })?;
    match entry.method {
        0 if entry.compressed_size == entry.size => Ok(compressed.to_vec()),
        0 => Err(format!(
            "the stored zip entry {} gives two different sizes",
            entry.label
        )
        .into()),
        8 => inflate(compressed, max_bytes),
        other => Err(format!(
            "the zip entry {} uses compression method {other}, which is not supported",
            entry.label
        )
        .into()),
    }
}

/// Inflates raw deflate data, failing once the output passes `max_bytes`.
fn inflate(input: &[u8], max_bytes: u64) -> Result<Vec<u8>, ReadError> {
    const CHUNK: usize = 64 * 1024;
    let cap = index(max_bytes)?;
    let mut inflater = Decompress::new(false);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let room = (cap + 1).saturating_sub(out.len()).min(CHUNK);
        if room == 0 {
            return Err(ReadError::TooLarge { size: None });
        }
        if out.capacity() - out.len() < room {
            // Grow geometrically, but never past the cap.
            let target = (out.len() * 2).max(out.len() + room).min(cap + 1);
            out.reserve_exact(target - out.len());
        }
        let consumed = index(inflater.total_in())?;
        let produced_before = out.len();
        let status = inflater
            .decompress_vec(
                &input[consumed.min(input.len())..],
                &mut out,
                FlushDecompress::None,
            )
            .map_err(|e| format!("a zip entry is damaged: {e}"))?;
        if out.len() > cap {
            return Err(ReadError::TooLarge { size: None });
        }
        match status {
            Status::StreamEnd => return Ok(out),
            Status::Ok | Status::BufError => {
                let progressed =
                    index(inflater.total_in())? != consumed || out.len() != produced_before;
                if !progressed {
                    return Err("a zip entry ends before its deflate stream does".into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, check_no_overlap};

    fn entry(name: &str, local_offset: u64, compressed_size: u64) -> Entry {
        Entry {
            name: name.to_string(),
            label: name.to_string(),
            method: 0,
            encrypted: false,
            compressed_size,
            size: compressed_size,
            local_offset,
        }
    }

    #[test]
    fn overlapping_entries_are_refused() {
        assert!(check_no_overlap(&[entry("a", 0, 10), entry("b", 41, 5)]).is_ok());
        assert!(check_no_overlap(&[entry("a", 0, 10), entry("b", 40, 5)]).is_err());
        assert!(check_no_overlap(&[entry("a", 0, 10), entry("b", 0, 10)]).is_err());
    }
}
