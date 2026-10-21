//! The OLE2 compound file (MS-CFB) layout, read with hard bounds, and the
//! mutations that damage it where readers have been weak: sector chains
//! (cycles, special values), directory entries (sizes, tree links), the
//! header, and the mini stream.

use super::le::{put_u16, put_u32, put_u64, u16_at, u32_at, u64_at};
use super::{INTERESTING_U32, le};
use crate::rng::Rng;

pub const MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
pub const FREE: u32 = 0xFFFF_FFFF;
pub const END: u32 = 0xFFFF_FFFE;
const FATSECT: u32 = 0xFFFF_FFFD;
const DIFSECT: u32 = 0xFFFF_FFFC;
const NOSTREAM: u32 = 0xFFFF_FFFF;
pub const DIR_ENTRY: usize = 128;

/// Where the parts of a compound file are. Built from possibly damaged
/// bytes: every chain stops at a repeat or at the end of the file.
#[derive(Debug, Clone)]
pub struct Layout {
    pub sector_size: usize,
    /// Sectors that hold the FAT, in order.
    pub fat_sectors: Vec<u32>,
    pub fat: Vec<u32>,
    /// File offset of each directory entry.
    pub dir_entries: Vec<usize>,
    /// Sectors that hold the mini FAT, in order.
    pub minifat_sectors: Vec<u32>,
    pub minifat: Vec<u32>,
    pub mini_cutoff: u64,
    pub version: u16,
}

impl Layout {
    pub fn parse(b: &[u8]) -> Option<Layout> {
        if b.len() < 512 || b[..8] != MAGIC {
            return None;
        }
        let shift = u16_at(b, 0x1E)?;
        if shift != 9 && shift != 12 {
            return None;
        }
        let ss = 1usize << shift;
        let sectors = b.len().saturating_sub(ss).div_ceil(ss);
        let mut layout = Layout {
            sector_size: ss,
            fat_sectors: Vec::new(),
            fat: Vec::new(),
            dir_entries: Vec::new(),
            minifat_sectors: Vec::new(),
            minifat: Vec::new(),
            mini_cutoff: u64::from(u32_at(b, 0x38)?),
            version: u16_at(b, 0x1A)?,
        };
        let num_fat = u32_at(b, 0x2C)? as usize;
        let want = num_fat.min(sectors);
        for i in 0..109 {
            let s = u32_at(b, 0x4C + 4 * i)?;
            if layout.fat_sectors.len() >= want || (s as usize) >= sectors {
                break;
            }
            layout.fat_sectors.push(s);
        }
        // DIFAT sectors beyond the header.
        let mut difat = u32_at(b, 0x44)?;
        let per = ss / 4 - 1;
        let mut seen = vec![false; sectors];
        while layout.fat_sectors.len() < want && (difat as usize) < sectors {
            if std::mem::replace(&mut seen[difat as usize], true) {
                break;
            }
            let at = (difat as usize + 1) * ss;
            for i in 0..per {
                let s = u32_at(b, at + 4 * i).unwrap_or(FREE);
                if layout.fat_sectors.len() < want && (s as usize) < sectors {
                    layout.fat_sectors.push(s);
                }
            }
            difat = u32_at(b, at + 4 * per).unwrap_or(END);
        }
        for &s in &layout.fat_sectors {
            let at = (s as usize + 1) * ss;
            for i in 0..ss / 4 {
                layout.fat.push(u32_at(b, at + 4 * i).unwrap_or(FREE));
            }
        }
        for s in chain(&layout.fat, u32_at(b, 0x30)?, sectors) {
            let at = (s as usize + 1) * ss;
            for i in 0..ss / DIR_ENTRY {
                if at + (i + 1) * DIR_ENTRY <= b.len() {
                    layout.dir_entries.push(at + i * DIR_ENTRY);
                }
            }
        }
        layout.minifat_sectors = chain(&layout.fat, u32_at(b, 0x3C)?, sectors);
        for &s in &layout.minifat_sectors {
            let at = (s as usize + 1) * ss;
            for i in 0..ss / 4 {
                layout.minifat.push(u32_at(b, at + 4 * i).unwrap_or(FREE));
            }
        }
        Some(layout)
    }

    /// File offset of a sector.
    pub fn sector_offset(&self, s: u32) -> usize {
        (s as usize + 1) * self.sector_size
    }

    /// File offset of FAT entry `i`.
    fn fat_entry_offset(&self, i: usize) -> Option<usize> {
        let per = self.sector_size / 4;
        let s = *self.fat_sectors.get(i / per)?;
        Some(self.sector_offset(s) + 4 * (i % per))
    }

    /// File offset of mini FAT entry `i`.
    fn minifat_entry_offset(&self, i: usize) -> Option<usize> {
        let per = self.sector_size / 4;
        let s = *self.minifat_sectors.get(i / per)?;
        Some(self.sector_offset(s) + 4 * (i % per))
    }

    /// Reads a stream by its directory entry, or `None` when its chain is
    /// broken or it is longer than `max` bytes.
    pub fn read_entry(
        &self,
        b: &[u8],
        entry: usize,
        ministream: &[u8],
        max: usize,
    ) -> Option<Vec<u8>> {
        let e = *self.dir_entries.get(entry)?;
        let start = u32_at(b, e + 0x74)?;
        let mut size = u64_at(b, e + 0x78)?;
        if self.version == 3 {
            size &= 0xFFFF_FFFF;
        }
        let size = usize::try_from(size).ok()?;
        if size == 0 {
            return Some(Vec::new());
        }
        let root = b[e + 0x42] == 5;
        let (unit, data, table): (usize, &[u8], &[u32]) =
            if !root && (size as u64) < self.mini_cutoff {
                (64, ministream, &self.minifat)
            } else {
                (self.sector_size, b, &self.fat)
            };
        if size > data.len() || size > max {
            return None;
        }
        let mut out = Vec::with_capacity(size);
        let limit = data.len() / unit + 1;
        for s in chain(table, start, limit) {
            if out.len() >= size {
                break;
            }
            let at = if unit == 64 {
                s as usize * 64
            } else {
                self.sector_offset(s)
            };
            let piece = data.get(at..(at + unit).min(data.len()))?;
            out.extend_from_slice(piece);
        }
        if out.len() < size {
            return None;
        }
        out.truncate(size);
        Some(out)
    }
}

/// Follows a chain from `start`, stopping at a special value, a sector at
/// or past `limit`, or a sector seen before.
pub fn chain(table: &[u32], start: u32, limit: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen = vec![false; table.len().min(limit)];
    let mut s = start;
    while (s as usize) < seen.len() {
        if std::mem::replace(&mut seen[s as usize], true) {
            break;
        }
        out.push(s);
        s = table[s as usize];
    }
    out
}

/// Damages the compound file. `None` when the input is not one.
pub fn mutate(rng: &mut Rng, b: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    let layout = Layout::parse(b)?;
    let mut out = b.to_vec();
    let op = match rng.below(9) {
        0 | 1 => {
            // A FAT entry: loop back, point at itself, or end early.
            let used: Vec<usize> = (0..layout.fat.len())
                .filter(|&i| layout.fat[i] < DIFSECT)
                .collect();
            let i = *used.get(rng.below(used.len()))?;
            let v = match rng.below(4) {
                0 => i as u32,
                1 => rng.below(i + 1) as u32,
                2 => rng.pick(&[FREE, END, FATSECT, DIFSECT, 0, 0x7FFF_FFFF]),
                _ => rng.below(layout.fat.len() + 8) as u32,
            };
            put_u32(&mut out, layout.fat_entry_offset(i)?, v);
            "cfb:fat_chain"
        }
        2 => {
            // A header field.
            let (at, v) = match rng.below(10) {
                0 => (0x1E, rng.pick(&[0u32, 7, 9, 12, 16, 31, 0xFFFF])),
                1 => (0x20, rng.pick(&[0u32, 3, 6, 12, 0xFFFF])),
                2 => (0x2C, rng.pick(INTERESTING_U32)),
                3 => (0x30, rng.pick(&[0u32, 1, FREE, END, 0x7FFF_FFFF])),
                4 => (0x38, rng.pick(&[0u32, 1, 64, 4095, 4096, 0xFFFF_FFFF])),
                5 => (0x3C, rng.pick(&[0u32, 1, FREE, END])),
                6 => (0x40, rng.pick(INTERESTING_U32)),
                7 => (0x44, rng.pick(&[0u32, 1, FREE, END])),
                8 => (0x48, rng.pick(INTERESTING_U32)),
                _ => (
                    0x4C + 4 * rng.below(109),
                    rng.pick(&[0u32, 1, FREE, END, FATSECT]),
                ),
            };
            if at == 0x1E || at == 0x20 {
                put_u16(&mut out, at, v as u16);
            } else {
                put_u32(&mut out, at, v);
            }
            "cfb:header"
        }
        3 | 4 => {
            // A stream size: around the mini cutoff, past the file, huge.
            let e = *layout
                .dir_entries
                .get(rng.below(layout.dir_entries.len()))?;
            let old = u64_at(b, e + 0x78)?;
            let v = match rng.below(8) {
                0 => 0,
                1 => 4095,
                2 => 4096,
                3 => old.saturating_mul(2).max(1),
                4 => old.saturating_add(1 + rng.below(4096) as u64),
                5 => u64::from(u32::MAX),
                6 => 1 << 40,
                _ => u64::MAX,
            };
            put_u64(&mut out, e + 0x78, v);
            "cfb:dir_size"
        }
        5 => {
            // Tree links: cycles, self links, out of range.
            let n = layout.dir_entries.len();
            let id = rng.below(n);
            let e = *layout.dir_entries.get(id)?;
            let field = e + rng.pick(&[0x44usize, 0x48, 0x4C]);
            let v = match rng.below(4) {
                0 => id as u32,
                1 => rng.below(n) as u32,
                2 => NOSTREAM,
                _ => rng.pick(INTERESTING_U32),
            };
            put_u32(&mut out, field, v);
            "cfb:dir_tree"
        }
        6 => {
            // Entry kind, name length, start sector, or a copy of another
            // entry (two entries with one name).
            let n = layout.dir_entries.len();
            let e = *layout.dir_entries.get(rng.below(n))?;
            match rng.below(4) {
                0 => out[e + 0x42] = rng.below(7) as u8,
                1 => put_u16(&mut out, e + 0x40, rng.pick(&[0u16, 1, 2, 64, 65, 0xFFFF])),
                2 => put_u32(&mut out, e + 0x74, rng.pick(INTERESTING_U32)),
                _ => {
                    let from = *layout.dir_entries.get(rng.below(n))?;
                    let copy = b.get(from..from + DIR_ENTRY)?.to_vec();
                    le::put(&mut out, e, &copy);
                }
            }
            "cfb:dir_entry"
        }
        7 => {
            // The mini FAT, or the root entry that holds the mini stream.
            if !layout.minifat.is_empty() && rng.one_in(2) {
                let i = rng.below(layout.minifat.len());
                let v = match rng.below(3) {
                    0 => i as u32,
                    1 => rng.below(i + 1) as u32,
                    _ => rng.pick(&[FREE, END, 0x7FFF_FFFF]),
                };
                put_u32(&mut out, layout.minifat_entry_offset(i)?, v);
            } else {
                let root = *layout.dir_entries.first()?;
                if rng.one_in(2) {
                    put_u32(&mut out, root + 0x74, rng.pick(INTERESTING_U32));
                } else {
                    put_u64(&mut out, root + 0x78, rng.pick(INTERESTING_U32).into());
                }
            }
            "cfb:mini_stream"
        }
        _ => {
            // A whole sector over another (shared sectors between chains).
            let ss = layout.sector_size;
            let sectors = (b.len() / ss).checked_sub(1)?;
            let from = layout.sector_offset(rng.below(sectors) as u32);
            let to = layout.sector_offset(rng.below(sectors) as u32);
            let copy = b.get(from..from + ss)?.to_vec();
            le::put(&mut out, to, &copy);
            "cfb:sector_copy"
        }
    };
    Some((out, op))
}
