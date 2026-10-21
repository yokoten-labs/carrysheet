//! Building zip archives byte by byte for tests, from APPNOTE's layouts:
//! well-formed ones, and ones whose headers lie. carrysheet-cli's tests
//! use this file too (by path).
#![allow(dead_code)]

use std::io::{Read, Seek, SeekFrom, Write};

use flate2::write::DeflateEncoder;
use flate2::{Compression, Crc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Stored,
    Deflated,
}

impl Method {
    pub fn code(self) -> u16 {
        match self {
            Method::Stored => 0,
            Method::Deflated => 8,
        }
    }
}

pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc::new();
    c.update(data);
    c.sum()
}

/// A deflate stream of `blocks` empty stored blocks and a final empty
/// block: 5 bytes each, no output.
pub fn empty_blocks(blocks: usize) -> Vec<u8> {
    let mut s = b"\x00\x00\x00\xff\xff".repeat(blocks);
    s.extend_from_slice(b"\x01\x00\x00\xff\xff");
    s
}

/// One entry as written: every field a header holds, so a test can make
/// the local and central headers disagree.
#[derive(Debug, Clone)]
pub struct Raw {
    pub name: Vec<u8>,
    pub local_name: Vec<u8>,
    pub flags: u16,
    pub local_flags: u16,
    pub method: u16,
    pub local_method: u16,
    pub crc: u32,
    /// The bytes after the local header, as stored.
    pub data: Vec<u8>,
    pub compressed_size: u32,
    pub size: u32,
    pub local_compressed_size: u32,
    pub local_size: u32,
    pub local_extra: Vec<u8>,
    pub central_extra: Vec<u8>,
}

impl Raw {
    pub fn new(name: &str, content: &[u8], method: Method) -> Raw {
        let data = match method {
            Method::Stored => content.to_vec(),
            Method::Deflated => deflate(content),
        };
        Raw {
            name: name.as_bytes().to_vec(),
            local_name: name.as_bytes().to_vec(),
            flags: 0,
            local_flags: 0,
            method: method.code(),
            local_method: method.code(),
            crc: crc32(content),
            compressed_size: data.len() as u32,
            size: content.len() as u32,
            local_compressed_size: data.len() as u32,
            local_size: content.len() as u32,
            data,
            local_extra: Vec::new(),
            central_extra: Vec::new(),
        }
    }

    /// Sets the flags in both headers.
    pub fn flags(mut self, flags: u16) -> Raw {
        self.flags = flags;
        self.local_flags = flags;
        self
    }

    pub fn local_header(&self) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"PK\x03\x04");
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(&self.local_flags.to_le_bytes());
        h.extend_from_slice(&self.local_method.to_le_bytes());
        h.extend_from_slice(&[0, 0, 0x21, 0]); // time, date
        h.extend_from_slice(&self.crc.to_le_bytes());
        h.extend_from_slice(&self.local_compressed_size.to_le_bytes());
        h.extend_from_slice(&self.local_size.to_le_bytes());
        h.extend_from_slice(&(self.local_name.len() as u16).to_le_bytes());
        h.extend_from_slice(&(self.local_extra.len() as u16).to_le_bytes());
        h.extend_from_slice(&self.local_name);
        h.extend_from_slice(&self.local_extra);
        h
    }

    pub fn central_header(&self, offset: u32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"PK\x01\x02");
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(&self.flags.to_le_bytes());
        h.extend_from_slice(&self.method.to_le_bytes());
        h.extend_from_slice(&[0, 0, 0x21, 0]);
        h.extend_from_slice(&self.crc.to_le_bytes());
        h.extend_from_slice(&self.compressed_size.to_le_bytes());
        h.extend_from_slice(&self.size.to_le_bytes());
        h.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        h.extend_from_slice(&(self.central_extra.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // comment
        h.extend_from_slice(&0u16.to_le_bytes()); // disk
        h.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        h.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        h.extend_from_slice(&offset.to_le_bytes());
        h.extend_from_slice(&self.name);
        h.extend_from_slice(&self.central_extra);
        h
    }
}

/// Writes entries one after another, then the central directory.
#[derive(Default)]
pub struct Builder {
    pub out: Vec<u8>,
    pub central: Vec<u8>,
    pub count: u64,
}

impl Builder {
    pub fn new() -> Builder {
        Builder::default()
    }

    pub fn add(&mut self, name: &str, content: &[u8], method: Method) -> &mut Builder {
        self.raw(Raw::new(name, content, method))
    }

    pub fn raw(&mut self, raw: Raw) -> &mut Builder {
        let offset = self.out.len() as u32;
        self.out.extend_from_slice(&raw.local_header());
        self.out.extend_from_slice(&raw.data);
        self.central_only(&raw, offset)
    }

    /// A central directory entry only, pointing at `offset`.
    pub fn central_only(&mut self, raw: &Raw, offset: u32) -> &mut Builder {
        self.central.extend_from_slice(&raw.central_header(offset));
        self.count += 1;
        self
    }

    /// The archive with a plain end record.
    pub fn finish(&self) -> Vec<u8> {
        let mut b = self.out.clone();
        let at = b.len() as u32;
        b.extend_from_slice(&self.central);
        b.extend_from_slice(&end_record(
            self.count as u16,
            self.central.len() as u32,
            at,
            0,
        ));
        b
    }

    /// The archive with zip64 end records (the plain one says "see zip64").
    pub fn finish_zip64(&self) -> Vec<u8> {
        let mut b = self.out.clone();
        let at = b.len() as u64;
        b.extend_from_slice(&self.central);
        let zip64_at = b.len() as u64;
        b.extend_from_slice(&zip64_end_record(self.count, self.central.len() as u64, at));
        b.extend_from_slice(&zip64_locator(zip64_at));
        b.extend_from_slice(&end_record(0xFFFF, u32::MAX, u32::MAX, 0));
        b
    }
}

pub fn zip(parts: &[(&str, &[u8], Method)]) -> Vec<u8> {
    let mut b = Builder::new();
    for (name, content, method) in parts {
        b.add(name, content, *method);
    }
    b.finish()
}

/// An end of central directory record (22 bytes, then the comment's
/// bytes are up to the caller).
pub fn end_record(count: u16, size: u32, offset: u32, comment_len: u16) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(b"PK\x05\x06");
    r.extend_from_slice(&[0, 0, 0, 0]);
    r.extend_from_slice(&count.to_le_bytes());
    r.extend_from_slice(&count.to_le_bytes());
    r.extend_from_slice(&size.to_le_bytes());
    r.extend_from_slice(&offset.to_le_bytes());
    r.extend_from_slice(&comment_len.to_le_bytes());
    r
}

/// A zip64 end of central directory record (56 bytes).
pub fn zip64_end_record(count: u64, size: u64, offset: u64) -> Vec<u8> {
    zip64_end_record_sized(44, count, size, offset)
}

/// Same, with any value in the record-size field.
pub fn zip64_end_record_sized(record_size: u64, count: u64, size: u64, offset: u64) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(b"PK\x06\x06");
    r.extend_from_slice(&record_size.to_le_bytes());
    r.extend_from_slice(&45u16.to_le_bytes());
    r.extend_from_slice(&45u16.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&count.to_le_bytes());
    r.extend_from_slice(&count.to_le_bytes());
    r.extend_from_slice(&size.to_le_bytes());
    r.extend_from_slice(&offset.to_le_bytes());
    r
}

/// A zip64 end of central directory locator (20 bytes).
pub fn zip64_locator(at: u64) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(b"PK\x06\x07");
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&at.to_le_bytes());
    r.extend_from_slice(&1u32.to_le_bytes());
    r
}

/// A zip64 extended information extra field with these values.
pub fn zip64_extra(values: &[u64]) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(&1u16.to_le_bytes());
    r.extend_from_slice(&((values.len() * 8) as u16).to_le_bytes());
    for v in values {
        r.extend_from_slice(&v.to_le_bytes());
    }
    r
}

/// A file of `len` bytes that is zeros except for the pieces written at
/// their offsets: a huge file without the disk space.
pub struct Sparse {
    len: u64,
    pieces: Vec<(u64, Vec<u8>)>,
    pos: u64,
}

impl Sparse {
    pub fn new(len: u64) -> Sparse {
        Sparse {
            len,
            pieces: Vec::new(),
            pos: 0,
        }
    }

    pub fn put(&mut self, at: u64, bytes: &[u8]) {
        self.pieces.push((at, bytes.to_vec()));
    }
}

impl std::fmt::Debug for Sparse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Sparse({} bytes, {} pieces)",
            self.len,
            self.pieces.len()
        )
    }
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos)) as usize;
        buf[..n].fill(0);
        for (at, bytes) in &self.pieces {
            let (start, end) = (*at, at + bytes.len() as u64);
            let (lo, hi) = (self.pos.max(start), (self.pos + n as u64).min(end));
            if lo < hi {
                let dst = (lo - self.pos) as usize..(hi - self.pos) as usize;
                buf[dst].copy_from_slice(&bytes[(lo - start) as usize..(hi - start) as usize]);
            }
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Sparse {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(n) => n,
            SeekFrom::End(d) => self.len.saturating_add_signed(d),
            SeekFrom::Current(d) => self.pos.saturating_add_signed(d),
        };
        Ok(self.pos)
    }
}

/// Writes deflate bits, least significant first (RFC 1951, 3.1.1).
#[derive(Default)]
pub struct Bits {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    pub fn new() -> Bits {
        Bits::default()
    }

    pub fn put(&mut self, v: u64, n: u32) {
        self.acc |= v << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code, most significant bit first.
    pub fn code(&mut self, c: u32, len: u32) {
        let reversed = (0..len).fold(0u64, |r, i| r | (u64::from(c >> i & 1) << (len - 1 - i)));
        self.put(reversed, len);
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let k = 8 - self.n;
            self.put(0, k);
        }
        self.out
    }

    /// A literal or length symbol in the fixed Huffman code (3.2.6).
    pub fn fixed(&mut self, sym: u32) {
        match sym {
            0..=143 => self.code(0x30 + sym, 8),
            144..=255 => self.code(0x190 + sym - 144, 9),
            256..=279 => self.code(sym - 256, 7),
            _ => self.code(0xC0 + sym - 280, 8),
        }
    }

    /// An empty, non-final fixed-Huffman block: 10 bits, no output.
    pub fn empty_fixed_block(&mut self) {
        self.put(0, 1);
        self.put(1, 2);
        self.fixed(256);
    }

    /// A final fixed block: `a`, then `matches` copies of 258 bytes at
    /// distance 1. Returns the bytes it inflates to.
    pub fn final_matches(&mut self, matches: usize) -> usize {
        self.put(1, 1);
        self.put(1, 2);
        self.fixed(u32::from(b'a'));
        for _ in 0..matches {
            self.fixed(285);
            self.code(0, 5);
        }
        self.fixed(256);
        1 + 258 * matches
    }
}

/// An entry whose deflate stream is `filler` bytes of empty fixed blocks
/// and then output made of matches (`matches_first` puts the output
/// before the filler instead). The sizes and CRC-32 are true.
pub fn empty_fixed_entry(name: &str, filler: usize, matches: usize, matches_first: bool) -> Raw {
    let mut b = Bits::new();
    let mut size = 0;
    if matches_first {
        // A non-final fixed block with the output, then the filler.
        b.put(0, 1);
        b.put(1, 2);
        b.fixed(u32::from(b'a'));
        for _ in 0..matches {
            b.fixed(285);
            b.code(0, 5);
        }
        b.fixed(256);
        size = 1 + 258 * matches;
    }
    while b.out.len() < filler {
        b.empty_fixed_block();
    }
    if matches_first {
        b.put(1, 1);
        b.put(1, 2);
        b.fixed(256);
    } else {
        size = b.final_matches(matches);
    }
    let data = b.finish();
    let content = vec![b'a'; size];
    let mut raw = Raw::new(name, b"", Method::Deflated);
    raw.crc = crc32(&content);
    raw.compressed_size = data.len() as u32;
    raw.local_compressed_size = data.len() as u32;
    raw.size = size as u32;
    raw.local_size = size as u32;
    raw.data = data;
    raw
}
