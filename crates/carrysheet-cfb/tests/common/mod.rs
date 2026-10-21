//! Shared helpers: fixtures, test data and byte patching.
#![allow(dead_code)]

use carrysheet_cfb::{Builder, Version};

pub fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The bytes the fixtures were made with (xorshift32, as in tests/data/README).
pub fn data(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// Every compound file in the workspace's test data, by name.
pub fn workspace_files() -> Vec<(String, Vec<u8>)> {
    let crates = format!("{}/..", env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    let mut dirs = vec![std::path::PathBuf::from(crates)];
    while let Some(dir) = dirs.pop() {
        let Ok(list) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in list.flatten() {
            let path = item.path();
            let name = path.to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                // Only the crates' test data, and never build output.
                if !name.ends_with("/target") {
                    dirs.push(path);
                }
            } else if name.contains("/tests/data/") {
                let bytes = std::fs::read(&path).unwrap();
                if carrysheet_cfb::is_compound_file(&bytes) {
                    let short = name.rsplit("/../").next().unwrap_or(&name).to_string();
                    out.push((short, bytes));
                }
            }
        }
    }
    out.sort();
    out
}

pub fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn set32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

pub fn set64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

pub fn sector_len(b: &[u8]) -> usize {
    1 << u16::from_le_bytes([b[30], b[31]])
}

/// Byte offset of a directory entry in a file this crate wrote (its
/// directory is contiguous).
pub fn entry_offset(b: &[u8], id: usize) -> usize {
    let ss = sector_len(b);
    (le32(b, 48) as usize + 1) * ss + id * 128
}

/// The id of the entry with this name, in a file this crate wrote.
pub fn entry_id(b: &[u8], name: &str) -> usize {
    let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    (0..)
        .find(|&id| {
            let at = entry_offset(b, id);
            b[at..at + units.len()] == units[..] && b[at + units.len()] == 0
        })
        .unwrap()
}

/// Byte offset of FAT entry `sector` (the first FAT sector only).
pub fn fat_offset(b: &[u8], sector: u32) -> usize {
    sector_len(b) + 4 * sector as usize
}

/// Byte offset of mini FAT entry `m` (the first mini FAT sector only).
pub fn minifat_offset(b: &[u8], m: u32) -> usize {
    (le32(b, 60) as usize + 1) * sector_len(b) + 4 * m as usize
}

/// A small file: a storage, a big stream and a small one.
pub fn sample(version: Version) -> Vec<u8> {
    let mut b = Builder::new(version);
    b.create_storage("/S").unwrap();
    b.create_stream("/Big", data(1, 10_000)).unwrap();
    b.create_stream("/Small", data(2, 300)).unwrap();
    b.create_stream("/S/Inner", data(3, 200)).unwrap();
    b.to_bytes().unwrap()
}

// ---- raw files, for structures the Builder never writes ----

pub const END: u32 = 0xFFFF_FFFE;
pub const FREE: u32 = 0xFFFF_FFFF;
pub const NOS: u32 = 0xFFFF_FFFF;

/// A raw directory entry.
#[derive(Clone)]
pub struct Raw {
    pub name: Vec<u16>,
    pub kind: u8,
    pub left: u32,
    pub right: u32,
    pub child: u32,
    pub start: u32,
    pub size: u64,
}

impl Raw {
    pub fn new(name: &str, kind: u8) -> Raw {
        Raw {
            name: name.encode_utf16().collect(),
            kind,
            left: NOS,
            right: NOS,
            child: NOS,
            start: END,
            size: 0,
        }
    }

    fn bytes(&self) -> [u8; 128] {
        let mut e = [0u8; 128];
        for (k, u) in self.name.iter().enumerate().take(31) {
            e[2 * k..2 * k + 2].copy_from_slice(&u.to_le_bytes());
        }
        let len = ((self.name.len().min(31) + 1) * 2) as u16;
        e[64..66].copy_from_slice(&len.to_le_bytes());
        e[66] = self.kind;
        e[67] = 1;
        e[68..72].copy_from_slice(&self.left.to_le_bytes());
        e[72..76].copy_from_slice(&self.right.to_le_bytes());
        e[76..80].copy_from_slice(&self.child.to_le_bytes());
        e[116..120].copy_from_slice(&self.start.to_le_bytes());
        e[120..128].copy_from_slice(&self.size.to_le_bytes());
        e
    }
}

/// A version 4 file: FAT sectors from 0, the directory right after, then
/// `extra` data sectors whose FAT entries come from `extra_fat(sector)`.
/// Returns (bytes, first directory sector, first extra sector).
pub fn raw_v4(
    entries: &[Raw],
    extra: usize,
    extra_fat: &dyn Fn(u32) -> u32,
) -> (Vec<u8>, u32, u32) {
    let ss = 4096usize;
    let eps = ss / 4;
    let dir = (entries.len() * 128).div_ceil(ss);
    let mut nfat = 1;
    while (nfat + dir + extra).div_ceil(eps) > nfat {
        nfat = (nfat + dir + extra).div_ceil(eps);
    }
    assert!(nfat <= 109);
    let total = nfat + dir + extra;
    let mut out = vec![0u8; (total + 1) * ss];
    let mut fat = vec![FREE; nfat * eps];
    for s in fat.iter_mut().take(nfat) {
        *s = 0xFFFF_FFFD;
    }
    let d0 = nfat as u32;
    for i in 0..dir {
        fat[nfat + i] = if i + 1 < dir { d0 + i as u32 + 1 } else { END };
    }
    let x0 = (nfat + dir) as u32;
    for i in 0..extra {
        let s = x0 + i as u32;
        fat[s as usize] = extra_fat(s);
    }
    for (i, w) in fat.iter().enumerate() {
        set32(&mut out, ss + i * 4, *w);
    }
    for (i, e) in entries.iter().enumerate() {
        let at = (1 + nfat) * ss + i * 128;
        out[at..at + 128].copy_from_slice(&e.bytes());
    }
    for i in entries.len()..dir * ss / 128 {
        let at = (1 + nfat) * ss + i * 128;
        out[at + 68..at + 80].fill(0xFF);
    }
    out[..8].copy_from_slice(&carrysheet_cfb::SIGNATURE);
    out[24..26].copy_from_slice(&0x3Eu16.to_le_bytes());
    out[26..28].copy_from_slice(&4u16.to_le_bytes());
    out[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes());
    out[30..32].copy_from_slice(&12u16.to_le_bytes());
    out[32..34].copy_from_slice(&6u16.to_le_bytes());
    set32(&mut out, 40, dir as u32);
    set32(&mut out, 44, nfat as u32);
    set32(&mut out, 48, d0);
    set32(&mut out, 56, 4096);
    set32(&mut out, 60, END);
    set32(&mut out, 68, END);
    for k in 0..109 {
        set32(&mut out, 76 + 4 * k, if k < nfat { k as u32 } else { FREE });
    }
    (out, d0, x0)
}

/// The fuzz harness file from review round 1: a tree stream and two
/// detached streams sharing one chain of `n` sectors.
pub fn shared_chain(n: usize) -> Vec<u8> {
    let mut es = vec![Raw::new("Root Entry", 5)];
    es[0].child = 1;
    let mut a = Raw::new("A", 2);
    a.size = (n * 4096) as u64;
    es.push(a);
    for name in ["D1", "D2"] {
        let mut d = Raw::new(name, 2);
        d.size = (n * 3 / 4 * 4096) as u64;
        es.push(d);
    }
    let (_, _, x0) = raw_v4(&es, n, &|_| END);
    for e in es.iter_mut().skip(1) {
        e.start = x0;
    }
    let last = x0 + n as u32 - 1;
    let mut b = raw_v4(&es, n, &move |s| if s == last { END } else { s + 1 }).0;
    for (i, x) in b.iter_mut().enumerate().skip(4096 * (1 + x0 as usize)) {
        *x = (i % 251) as u8;
    }
    b
}
