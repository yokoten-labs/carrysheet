//! Physical records, CONTINUE chains, and a reader that spans them.
//!
//! Every BIFF version shares one record header: a 2-byte type and a 2-byte
//! length, then the body. Bodies longer than the version's maximum continue in
//! CONTINUE (0x003C) records.

use crate::codepage;
use crate::error::{BiffError, BiffErrorKind};

pub(crate) const CONTINUE: u16 = 0x003C;

/// One physical record.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rec<'a> {
    pub typ: u16,
    pub data: &'a [u8],
    /// Stream offset of the record header.
    pub offset: u64,
}

impl Rec<'_> {
    /// Stream offset of the first body byte.
    pub(crate) fn body_offset(&self) -> u64 {
        self.offset + 4
    }
}

/// Walks the physical records of a stream from a starting offset.
pub(crate) struct Records<'a> {
    stream: &'a [u8],
    pos: usize,
}

impl<'a> Records<'a> {
    pub(crate) fn new(stream: &'a [u8], pos: usize) -> Self {
        Records { stream, pos }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// The next record, `None` at the end of the stream, or an error if the
    /// stream ends inside a header or body.
    pub(crate) fn next_rec(&mut self) -> Result<Option<Rec<'a>>, BiffError> {
        let rest = &self.stream[self.pos.min(self.stream.len())..];
        if rest.is_empty() {
            return Ok(None);
        }
        if rest.len() < 4 {
            return Err(BiffError::at(
                BiffErrorKind::UnexpectedEnd {
                    detail: format!(
                        "a record header needs 4 bytes but only {} remain",
                        rest.len()
                    ),
                },
                self.pos as u64,
            ));
        }
        let typ = u16::from_le_bytes([rest[0], rest[1]]);
        let len = u16::from_le_bytes([rest[2], rest[3]]) as usize;
        if rest.len() < 4 + len {
            return Err(BiffError::at(
                BiffErrorKind::UnexpectedEnd {
                    detail: format!(
                        "record 0x{typ:04X} declares {len} bytes but only {} remain",
                        rest.len() - 4
                    ),
                },
                self.pos as u64,
            ));
        }
        let rec = Rec {
            typ,
            data: &rest[4..4 + len],
            offset: self.pos as u64,
        };
        self.pos += 4 + len;
        Ok(Some(rec))
    }

    /// Type of the next record without consuming it.
    pub(crate) fn peek_type(&self) -> Option<u16> {
        let rest = self.stream.get(self.pos..)?;
        if rest.len() < 4 {
            return None;
        }
        Some(u16::from_le_bytes([rest[0], rest[1]]))
    }
}

/// A record body together with the CONTINUE bodies that follow it.
///
/// Reads may cross from one part to the next. Character data in BIFF8
/// strings is special: when it crosses a boundary, the new part starts with a
/// fresh option byte (see [`CRd::chars`]).
pub(crate) struct CRd<'a> {
    parts: Vec<Rec<'a>>,
    part: usize,
    pos: usize,
    /// Bytes in the parts before `part`, and in all parts: kept so that
    /// `remaining` does not walk every part (an SST can have thousands).
    before: usize,
    total: usize,
    pub(crate) typ: u16,
    pub(crate) name: &'static str,
}

impl<'a> CRd<'a> {
    pub(crate) fn new(parts: Vec<Rec<'a>>, name: &'static str) -> Self {
        let typ = parts.first().map(|p| p.typ).unwrap_or(0);
        let total = parts.iter().map(|p| p.data.len()).sum();
        CRd {
            parts,
            part: 0,
            pos: 0,
            before: 0,
            total,
            typ,
            name,
        }
    }

    pub(crate) fn single(data: &'a [u8], typ: u16, name: &'static str, body_offset: u64) -> Self {
        CRd::new(
            vec![Rec {
                typ,
                data,
                offset: body_offset.saturating_sub(4),
            }],
            name,
        )
    }

    /// Stream offset of the next byte.
    pub(crate) fn offset(&self) -> u64 {
        match self.parts.get(self.part) {
            Some(p) => p.body_offset() + self.pos as u64,
            None => self
                .parts
                .last()
                .map(|p| p.body_offset() + p.data.len() as u64)
                .unwrap_or(0),
        }
    }

    fn cur(&self) -> &'a [u8] {
        match self.parts.get(self.part) {
            Some(p) => &p.data[self.pos.min(p.data.len())..],
            None => &[],
        }
    }

    /// Bytes left in the current part.
    pub(crate) fn left_in_part(&self) -> usize {
        self.cur().len()
    }

    /// Moves to the next non-empty part when the current one is used up.
    /// Returns false if no data is left at all.
    fn settle(&mut self) -> bool {
        while self.part < self.parts.len() && self.cur().is_empty() {
            self.next_part();
        }
        self.part < self.parts.len()
    }

    fn next_part(&mut self) {
        if let Some(p) = self.parts.get(self.part) {
            self.before += p.data.len();
        }
        self.part += 1;
        self.pos = 0;
    }

    /// True when every part is used up.
    pub(crate) fn is_done(&mut self) -> bool {
        !self.settle()
    }

    /// Total bytes left across all parts.
    pub(crate) fn remaining(&self) -> usize {
        let used_here = match self.parts.get(self.part) {
            Some(p) => self.pos.min(p.data.len()),
            None => 0,
        };
        self.total
            .saturating_sub(self.before.saturating_add(used_here))
    }

    pub(crate) fn truncated(&self, needed: usize) -> BiffError {
        let total = self.total;
        BiffError::at(
            BiffErrorKind::Truncated {
                record: self.name,
                record_type: self.typ,
                needed: (total - self.remaining()).saturating_add(needed),
                available: total,
            },
            self.offset(),
        )
    }

    pub(crate) fn malformed(&self, detail: impl Into<String>) -> BiffError {
        BiffError::at(
            BiffErrorKind::Malformed {
                record: self.name,
                record_type: self.typ,
                detail: detail.into(),
            },
            self.offset(),
        )
    }

    pub(crate) fn u8(&mut self) -> Result<u8, BiffError> {
        if !self.settle() {
            return Err(self.truncated(1));
        }
        let b = self.cur()[0];
        self.pos += 1;
        Ok(b)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, BiffError> {
        let a = self.u8()?;
        let b = self.u8()?;
        Ok(u16::from_le_bytes([a, b]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, BiffError> {
        let a = self.u16()? as u32;
        let b = self.u16()? as u32;
        Ok(a | (b << 16))
    }

    /// Copies `n` bytes, crossing parts as needed.
    pub(crate) fn bytes(&mut self, n: usize) -> Result<Vec<u8>, BiffError> {
        if n > self.remaining() {
            return Err(self.truncated(n));
        }
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            self.settle();
            let k = (n - out.len()).min(self.left_in_part());
            out.extend_from_slice(&self.cur()[..k]);
            self.pos += k;
        }
        Ok(out)
    }

    /// Reads `cch` BIFF8 characters. `high` says whether they start as
    /// UTF-16 (true) or compressed Latin-1 (false). When the characters cross
    /// into a CONTINUE part, that part begins with a new option byte whose
    /// bit 0 gives the encoding for the rest of the string.
    pub(crate) fn chars(&mut self, cch: usize, mut high: bool) -> Result<String, BiffError> {
        let mut out = String::new();
        let mut left = cch;
        while left > 0 {
            if self.left_in_part() == 0 {
                // The characters go on in the next part, which starts with
                // an option byte (Apache POI and calamine read it the same way).
                if self.part + 1 >= self.parts.len() {
                    return Err(self.truncated(if high { left * 2 } else { left }));
                }
                self.next_part();
                let flag = self.u8()?;
                if flag & 0xFE != 0 {
                    return Err(self.malformed(format!(
                        "a string continues with option byte 0x{flag:02X}; only 0x00 or 0x01 is valid"
                    )));
                }
                high = flag & 1 != 0;
                continue;
            }
            let width = if high { 2 } else { 1 };
            let avail = self.left_in_part() / width;
            if avail == 0 {
                return Err(
                    self.malformed("a UTF-16 character is split across a CONTINUE boundary")
                );
            }
            let k = left.min(avail);
            let raw = &self.cur()[..k * width];
            if high {
                out.push_str(&codepage::utf16le(raw));
            } else {
                out.push_str(&codepage::latin1(raw));
            }
            self.pos += k * width;
            left -= k;
        }
        Ok(out)
    }
}

/// Collects `first` and the CONTINUE records after it, within `max_parts`.
pub(crate) fn with_continues<'a>(
    first: Rec<'a>,
    recs: &mut Records<'a>,
    max_parts: usize,
    count: &mut usize,
    max_records: usize,
) -> Result<Vec<Rec<'a>>, BiffError> {
    let mut parts = vec![first];
    while recs.peek_type() == Some(CONTINUE) {
        if parts.len() >= max_parts {
            return Err(BiffError::limit(
                "CONTINUE records after one record",
                max_parts,
                Some(first.offset),
            ));
        }
        *count += 1;
        if *count > max_records {
            return Err(BiffError::limit(
                "records",
                max_records,
                Some(recs.pos() as u64),
            ));
        }
        match recs.next_rec()? {
            Some(r) => parts.push(r),
            None => break,
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(data: &[u8]) -> Rec<'_> {
        Rec {
            typ: 0x00FC,
            data,
            offset: 0,
        }
    }

    #[test]
    fn chars_switch_compression_at_boundary() {
        // "AB" compressed, then the CONTINUE flips to UTF-16 for "中".
        let a = *b"AB";
        let b = [0x01, 0x2D, 0x4E];
        let mut r = CRd::new(vec![rec(&a), rec(&b)], "SST");
        assert_eq!(r.chars(3, false).unwrap(), "AB中");
        assert!(r.is_done());
    }

    #[test]
    fn plain_reads_cross_boundaries() {
        let a = [0x34];
        let b = [0x12, 0x01];
        let mut r = CRd::new(vec![rec(&a), rec(&b)], "SST");
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u8().unwrap(), 1);
        assert!(r.u8().is_err());
    }

    #[test]
    fn short_header_is_an_error() {
        let mut recs = Records::new(&[0x09, 0x08, 0x10], 0);
        assert!(recs.next_rec().is_err());
        let mut recs = Records::new(&[0x09, 0x08, 0x10, 0x00, 0x00], 0);
        assert!(recs.next_rec().is_err());
    }
}
