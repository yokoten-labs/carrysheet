//! A bounds-checked little-endian cursor over one record's bytes.

use crate::error::{BiffError, BiffErrorKind};

/// Reads fields from a record body. Every read is checked; a short record
/// becomes a [`BiffErrorKind::Truncated`] error naming the record and offset.
#[derive(Clone)]
pub(crate) struct Rd<'a> {
    data: &'a [u8],
    pos: usize,
    /// Record type, for error messages.
    pub(crate) typ: u16,
    /// Record name, for error messages.
    pub(crate) name: &'static str,
    /// Stream offset of `data[0]`.
    pub(crate) base: u64,
}

impl<'a> Rd<'a> {
    pub(crate) fn new(data: &'a [u8], typ: u16, name: &'static str, base: u64) -> Self {
        Rd {
            data,
            pos: 0,
            typ,
            name,
            base,
        }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    pub(crate) fn offset(&self) -> u64 {
        self.base + self.pos as u64
    }

    pub(crate) fn truncated(&self, needed: usize) -> BiffError {
        BiffError::at(
            BiffErrorKind::Truncated {
                record: self.name,
                record_type: self.typ,
                needed: self.pos.saturating_add(needed),
                available: self.data.len(),
            },
            self.offset(),
        )
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], BiffError> {
        if self.remaining() < n {
            return Err(self.truncated(n));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<(), BiffError> {
        self.take(n).map(|_| ())
    }

    pub(crate) fn u8(&mut self) -> Result<u8, BiffError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, BiffError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, BiffError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn f64(&mut self) -> Result<f64, BiffError> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(f64::from_le_bytes(a))
    }
}

pub(crate) fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}
