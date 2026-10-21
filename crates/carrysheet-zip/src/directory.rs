//! The end records and the central directory (APPNOTE 4.3.12 to 4.3.16).

use std::io::{Read, Seek, SeekFrom};

use crate::error::{Error, damaged, limit};
use crate::{Entry, FLAG_UTF8, Limits, Method, cp437};

const END_SIGNATURE: u32 = 0x0605_4b50;
const END_LEN: usize = 22;
/// The end record and the longest comment: where the end record must be.
pub(crate) const END_REACH: u64 = END_LEN as u64 + 65_535;
const LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const LOCATOR_LEN: u64 = 20;
const ZIP64_END_SIGNATURE: u32 = 0x0606_4b50;
const ZIP64_END_LEN: u64 = 56;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const CENTRAL_LEN: usize = 46;
pub(crate) const LOCAL_LEN: u64 = 30;
/// The zip64 extended information extra field (APPNOTE 4.5.3).
const ZIP64_EXTRA: u16 = 0x0001;

pub(crate) fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

pub(crate) fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub(crate) fn le64(b: &[u8], at: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}

fn read_at<R: Read + Seek>(reader: &mut R, at: u64, buf: &mut [u8]) -> Result<(), Error> {
    reader.seek(SeekFrom::Start(at))?;
    reader.read_exact(buf)?;
    Ok(())
}

/// What the end records say.
struct End {
    disk: u32,
    directory_disk: u32,
    entries_on_disk: u64,
    entries: u64,
    directory_size: u64,
    directory_offset: u64,
    /// Where the central directory must end: the zip64 end record, or
    /// the end record.
    directory_end: u64,
}

/// Reads the end records and the central directory, and checks the
/// entries' places in the file.
pub(crate) fn read<R: Read + Seek>(reader: &mut R, limits: &Limits) -> Result<Vec<Entry>, Error> {
    let end = find_end(reader)?;
    if end.disk != 0 || end.directory_disk != 0 || end.entries_on_disk != end.entries {
        return Err(Error::Unsupported(
            "the archive is split over several disks".into(),
        ));
    }
    if end.entries > limits.max_entries as u64 {
        return Err(limit("zip entries", limits.max_entries as u64));
    }
    if end.directory_size > limits.max_directory_bytes {
        return Err(limit(
            "bytes of the central directory",
            limits.max_directory_bytes,
        ));
    }
    if end.directory_offset.checked_add(end.directory_size) != Some(end.directory_end) {
        return Err(damaged(
            "the central directory does not end where the end record begins (data before the archive, or a wrong offset)",
        ));
    }
    if end.directory_size < end.entries * CENTRAL_LEN as u64 {
        return Err(damaged(
            "the central directory is too small for its entry count",
        ));
    }
    // Both are bounded now: the size by the limit, the count by the size.
    let mut directory = vec![0u8; end.directory_size as usize];
    read_at(reader, end.directory_offset, &mut directory)?;
    let mut entries = parse_directory(&directory, end.entries as usize)?;
    place_entries(&mut entries, end.directory_offset)?;
    Ok(entries)
}

/// Finds the last valid end record in the last [`END_REACH`] bytes, in
/// one backward search, and the zip64 end record its locator names.
fn find_end<R: Read + Seek>(reader: &mut R) -> Result<End, Error> {
    let len = reader.seek(SeekFrom::End(0))?;
    if len < END_LEN as u64 {
        return Err(damaged("the file is too short to be a zip"));
    }
    let tail_len = len.min(END_REACH) as usize;
    let tail_start = len - tail_len as u64;
    let mut tail = vec![0u8; tail_len];
    read_at(reader, tail_start, &mut tail)?;
    // Valid: the signature, and the comment it declares fits the file.
    let found = (0..=tail_len - END_LEN).rev().find(|&i| {
        le32(&tail, i) == END_SIGNATURE
            && i + END_LEN + usize::from(le16(&tail, i + 20)) <= tail_len
    });
    let Some(i) = found else {
        return Err(damaged(
            "no end-of-central-directory record in the last 65,557 bytes",
        ));
    };
    let r = &tail[i..i + END_LEN];
    let end_at = tail_start + i as u64;
    let mut end = End {
        disk: u32::from(le16(r, 4)),
        directory_disk: u32::from(le16(r, 6)),
        entries_on_disk: u64::from(le16(r, 8)),
        entries: u64::from(le16(r, 10)),
        directory_size: u64::from(le32(r, 12)),
        directory_offset: u64::from(le32(r, 16)),
        directory_end: end_at,
    };

    // A zip64 locator, if any, is right before the end record.
    let Some(locator_at) = end_at.checked_sub(LOCATOR_LEN) else {
        return Ok(end);
    };
    let mut locator = [0u8; LOCATOR_LEN as usize];
    read_at(reader, locator_at, &mut locator)?;
    if le32(&locator, 0) != LOCATOR_SIGNATURE {
        return Ok(end);
    }
    if le32(&locator, 4) != 0 || le32(&locator, 16) > 1 {
        return Err(Error::Unsupported(
            "the archive is split over several disks".into(),
        ));
    }
    let zip64_at = le64(&locator, 8);
    if zip64_at
        .checked_add(ZIP64_END_LEN)
        .is_none_or(|e| e > locator_at)
    {
        return Err(damaged("the zip64 locator points at or past itself"));
    }
    let mut record = [0u8; ZIP64_END_LEN as usize];
    read_at(reader, zip64_at, &mut record)?;
    if le32(&record, 0) != ZIP64_END_SIGNATURE {
        return Err(damaged(
            "the zip64 end record is not where its locator points",
        ));
    }
    // The record (12 bytes plus its declared size) ends at the locator.
    if le64(&record, 4).checked_add(12) != Some(locator_at - zip64_at) {
        return Err(damaged(
            "the zip64 end record's size does not reach its locator",
        ));
    }
    end.disk = le32(&record, 16);
    end.directory_disk = le32(&record, 20);
    end.entries_on_disk = le64(&record, 24);
    end.entries = le64(&record, 32);
    end.directory_size = le64(&record, 40);
    end.directory_offset = le64(&record, 48);
    end.directory_end = zip64_at;
    Ok(end)
}

/// Parses exactly `count` central directory headers filling `directory`.
fn parse_directory(directory: &[u8], count: usize) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::with_capacity(count);
    let mut at = 0usize;
    for i in 0..count {
        let Some(h) = directory.get(at..at + CENTRAL_LEN) else {
            return Err(damaged(
                "the central directory is shorter than its entry count",
            ));
        };
        if le32(h, 0) != CENTRAL_SIGNATURE {
            return Err(damaged(format!(
                "central directory entry {i} has no header signature"
            )));
        }
        let flags = le16(h, 8);
        let method = Method::from_code(le16(h, 10));
        let crc32 = le32(h, 16);
        let mut compressed_size = u64::from(le32(h, 20));
        let mut size = u64::from(le32(h, 24));
        let name_len = usize::from(le16(h, 28));
        let extra_len = usize::from(le16(h, 30));
        let comment_len = usize::from(le16(h, 32));
        let mut disk = u32::from(le16(h, 34));
        let mut header_offset = u64::from(le32(h, 42));
        let name_at = at + CENTRAL_LEN;
        let extra_at = name_at + name_len;
        let next = extra_at + extra_len + comment_len;
        if next > directory.len() {
            return Err(damaged(format!(
                "central directory entry {i} runs past the directory"
            )));
        }
        let raw_name = &directory[name_at..extra_at];
        let extra = &directory[extra_at..extra_at + extra_len];
        let mut fields = Zip64Fields {
            size: (size == u64::from(u32::MAX)).then_some(&mut size),
            compressed_size: (compressed_size == u64::from(u32::MAX))
                .then_some(&mut compressed_size),
            header_offset: (header_offset == u64::from(u32::MAX)).then_some(&mut header_offset),
            disk: (disk == u32::from(u16::MAX)).then_some(&mut disk),
        };
        if !fields.read(extra) {
            return Err(damaged(format!(
                "central directory entry {i} needs zip64 sizes it does not have"
            )));
        }
        if disk != 0 {
            return Err(Error::Unsupported(
                "the archive is split over several disks".into(),
            ));
        }
        let name = decode_name(raw_name, flags).ok_or_else(|| {
            damaged(format!(
                "entry {i}'s name is marked UTF-8 but is not valid UTF-8"
            ))
        })?;
        entries.push(Entry {
            name,
            method,
            flags,
            crc32,
            compressed_size,
            size,
            header_offset,
            raw_name: raw_name.into(),
            data_limit: 0,
        });
        at = next;
    }
    if at != directory.len() {
        return Err(damaged(
            "the central directory holds more than its entry count",
        ));
    }
    Ok(entries)
}

/// The values a zip64 extra field replaces: those set to all ones in the
/// header, in APPNOTE's order.
pub(crate) struct Zip64Fields<'a> {
    pub size: Option<&'a mut u64>,
    pub compressed_size: Option<&'a mut u64>,
    pub header_offset: Option<&'a mut u64>,
    pub disk: Option<&'a mut u32>,
}

impl Zip64Fields<'_> {
    /// Fills the fields from the zip64 extra field; false when one is
    /// needed and missing. Linear in the extra field.
    pub fn read(&mut self, extra: &[u8]) -> bool {
        let wanted = self.size.is_some()
            || self.compressed_size.is_some()
            || self.header_offset.is_some()
            || self.disk.is_some();
        if !wanted {
            return true;
        }
        let mut at = 0;
        while at + 4 <= extra.len() {
            let id = le16(extra, at);
            let len = usize::from(le16(extra, at + 2));
            let Some(data) = extra.get(at + 4..at + 4 + len) else {
                return false;
            };
            if id == ZIP64_EXTRA {
                let mut pos = 0;
                let mut take = |n: usize| {
                    let v = data.get(pos..pos + n)?;
                    pos += n;
                    Some(v)
                };
                for field in [
                    self.size.as_deref_mut(),
                    self.compressed_size.as_deref_mut(),
                    self.header_offset.as_deref_mut(),
                ]
                .into_iter()
                .flatten()
                {
                    let Some(v) = take(8) else { return false };
                    *field = le64(v, 0);
                }
                if let Some(disk) = self.disk.as_deref_mut() {
                    let Some(v) = take(4) else { return false };
                    *disk = le32(v, 0);
                }
                return true;
            }
            at += 4 + len;
        }
        false
    }
}

/// UTF-8 when flagged (None if invalid), else UTF-8 when valid, else
/// code page 437.
fn decode_name(raw: &[u8], flags: u16) -> Option<String> {
    match std::str::from_utf8(raw) {
        Ok(s) => Some(s.to_owned()),
        Err(_) if flags & FLAG_UTF8 != 0 => None,
        Err(_) => Some(cp437::decode(raw)),
    }
}

/// Sorts the entries by where they start and checks that none overlaps
/// the next or the central directory, and that the first starts at 0.
/// Sets each entry's data limit.
fn place_entries(entries: &mut [Entry], directory_offset: u64) -> Result<(), Error> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| entries[i].header_offset);
    let first = order
        .first()
        .map_or(directory_offset, |&i| entries[i].header_offset);
    if first != 0 {
        return Err(damaged("there is data before the first entry"));
    }
    for (k, &i) in order.iter().enumerate() {
        let next = order
            .get(k + 1)
            .map_or(directory_offset, |&j| entries[j].header_offset);
        let e = &entries[i];
        // The least an entry takes: its local header, name and data.
        let least = e
            .header_offset
            .checked_add(LOCAL_LEN + e.raw_name.len() as u64)
            .and_then(|x| x.checked_add(e.compressed_size));
        if least.is_none_or(|end| end > next) {
            return Err(damaged(match order.get(k + 1) {
                Some(&j) => format!("entries {i} and {j} overlap"),
                None => format!("entry {i} runs into the central directory"),
            }));
        }
        entries[i].data_limit = next;
    }
    Ok(())
}
