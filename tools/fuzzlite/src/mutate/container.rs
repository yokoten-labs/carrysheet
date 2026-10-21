//! The streams of a compound file, so a format-aware mutation can change a
//! stream (and its length) and get a well-formed file back.
//!
//! Reading uses the bounded [`Layout`]; seeds can be hostile test files, so
//! the `cfb` crate is only used to write.

use std::io::{Cursor, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::cfb::{DIR_ENTRY, Layout};
use super::le::{u16_at, u32_at};

/// The storages and streams of a compound file, parents first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// 3 (512-byte sectors) or 4 (4096-byte sectors).
    pub version: u16,
    pub nodes: Vec<Node>,
}

/// One storage or stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Path from the root, such as `_VBA_PROJECT_CUR/VBA/dir`.
    pub path: String,
    pub storage: bool,
    pub data: Vec<u8>,
}

impl Container {
    /// Reads every stream. `None` when the bytes are not a compound file,
    /// a chain is broken, or a name cannot be written back.
    pub fn parse(b: &[u8]) -> Option<Container> {
        let layout = Layout::parse(b)?;
        let root = *layout.dir_entries.first()?;
        if b[root + 0x42] != 5 {
            return None;
        }
        // Directory entries can all claim one long chain; cap the bytes kept
        // over all streams so a hostile seed cannot make this quadratic.
        let mut budget = b.len().saturating_mul(4);
        let ministream = layout.read_entry(b, 0, &[], budget).unwrap_or_default();
        budget -= ministream.len();
        let n = layout.dir_entries.len();
        let mut seen = vec![false; n];
        seen[0] = true;
        let mut nodes = Vec::new();
        let mut stack = vec![(u32_at(b, root + 0x4C)?, String::new())];
        while let Some((id, parent)) = stack.pop() {
            let id = id as usize;
            if id >= n || std::mem::replace(&mut seen[id], true) {
                continue;
            }
            let e = layout.dir_entries[id];
            stack.push((u32_at(b, e + 0x44)?, parent.clone()));
            stack.push((u32_at(b, e + 0x48)?, parent.clone()));
            let name = entry_name(b, e)?;
            let path = if parent.is_empty() {
                name
            } else {
                format!("{parent}/{name}")
            };
            match b[e + 0x42] {
                1 => {
                    stack.push((u32_at(b, e + 0x4C)?, path.clone()));
                    nodes.push(Node {
                        path,
                        storage: true,
                        data: Vec::new(),
                    });
                }
                2 => {
                    let data = layout.read_entry(b, id, &ministream, budget)?;
                    budget -= data.len();
                    nodes.push(Node {
                        path,
                        storage: false,
                        data,
                    });
                }
                _ => {}
            }
        }
        Some(Container {
            version: layout.version,
            nodes,
        })
    }

    /// The first stream whose lowercase path passes `pred`.
    pub fn find(&self, pred: impl Fn(&str) -> bool) -> Option<usize> {
        self.nodes
            .iter()
            .position(|n| !n.storage && pred(&n.path.to_lowercase()))
    }

    /// Indexes of every stream whose lowercase path passes `pred`.
    pub fn find_all(&self, pred: impl Fn(&str) -> bool) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|&i| !self.nodes[i].storage && pred(&self.nodes[i].path.to_lowercase()))
            .collect()
    }

    /// A copy with stream `index` replaced, written as a compound file.
    pub fn with_stream(&self, index: usize, data: Vec<u8>) -> Option<Vec<u8>> {
        let mut copy = self.clone();
        copy.nodes.get_mut(index)?.data = data;
        copy.build()
    }

    /// Writes the storages and streams as a new compound file.
    pub fn build(&self) -> Option<Vec<u8>> {
        catch_unwind(AssertUnwindSafe(|| self.build_inner()))
            .ok()
            .flatten()
    }

    fn build_inner(&self) -> Option<Vec<u8>> {
        let version = if self.version == 4 {
            cfb::Version::V4
        } else {
            cfb::Version::V3
        };
        let mut file =
            cfb::CompoundFile::create_with_version(version, Cursor::new(Vec::new())).ok()?;
        for node in &self.nodes {
            let path = format!("/{}", node.path);
            if node.storage {
                file.create_storage(&path).ok()?;
            } else {
                let mut stream = file.create_stream(&path).ok()?;
                stream.write_all(&node.data).ok()?;
            }
        }
        file.flush().ok()?;
        let mut out = file.into_inner().into_inner();
        // `cfb` stamps creation and modification times; zero them so one
        // `--seed` always gives the same bytes and finding ids.
        let layout = Layout::parse(&out)?;
        for e in layout.dir_entries {
            out[e + 0x64..e + 0x74].fill(0);
        }
        Some(out)
    }
}

/// The name of a directory entry, if `cfb` can write it back.
fn entry_name(b: &[u8], e: usize) -> Option<String> {
    let len = usize::from(u16_at(b, e + 0x40)?).min(64);
    let units: Vec<u16> = b
        .get(e..e + len.saturating_sub(2).min(DIR_ENTRY))?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    let name = String::from_utf16(&units).ok()?;
    let bad = name.is_empty() || name.contains(['/', '\\', ':', '!']);
    (!bad).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(path: &str) -> Vec<u8> {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        std::fs::read(format!("{root}/{path}")).unwrap()
    }

    #[test]
    fn reads_and_rebuilds_a_workbook() {
        let bytes = data("crates/carrysheet-biff/tests/data/SimpleMultiCell.xls");
        let c = Container::parse(&bytes).expect("parses");
        let wb = c.find(|p| p == "workbook").expect("has a Workbook stream");
        let rebuilt = c.build().expect("builds");
        let again = Container::parse(&rebuilt).expect("rebuilt file parses");
        assert_eq!(again.nodes.len(), c.nodes.len());
        assert_eq!(again.nodes[wb].data, c.nodes[wb].data);
        // The rebuilt file reads the same.
        let a = carrysheet_biff::read(&bytes).unwrap();
        let b = carrysheet_biff::read(&rebuilt).unwrap();
        assert_eq!(a.sheets, b.sheets);
    }

    #[test]
    fn reads_a_vba_storage_with_mini_streams() {
        let bytes = data("crates/carrysheet-vba/tests/data/SimpleMacro.xls");
        let c = Container::parse(&bytes).expect("parses");
        let dir = c
            .find(|p| p.ends_with("vba/dir"))
            .expect("has a VBA/dir stream");
        assert!(c.nodes[dir].path.starts_with("_VBA_PROJECT_CUR/"));
        let rebuilt = c.build().unwrap();
        let before = carrysheet_vba::read_xls_vba(&bytes).unwrap().unwrap();
        let after = carrysheet_vba::read_xls_vba(&rebuilt).unwrap().unwrap();
        assert_eq!(before.modules, after.modules);
    }

    /// A compound file whose `d` data sectors double as the directory:
    /// `4 * d - 1` stream entries all claim the same `d`-sector chain.
    /// (The review's probe, ported from its quadratic-seed.mjs.)
    fn shared_chain_file(d: usize) -> Vec<u8> {
        let mut f = 1;
        while f * 128 < d + f {
            f += 1;
        }
        let mut b = vec![0u8; 512 + (d + f) * 512];
        b[..8].copy_from_slice(&super::super::cfb::MAGIC);
        let put16 =
            |b: &mut [u8], at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_le_bytes());
        let put32 =
            |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put16(&mut b, 0x18, 0x3E);
        put16(&mut b, 0x1A, 3);
        put16(&mut b, 0x1C, 0xFFFE);
        put16(&mut b, 0x1E, 9);
        put16(&mut b, 0x20, 6);
        put32(&mut b, 0x2C, f as u32);
        put32(&mut b, 0x30, 0);
        put32(&mut b, 0x38, 4096);
        put32(&mut b, 0x3C, 0xFFFF_FFFE);
        put32(&mut b, 0x44, 0xFFFF_FFFE);
        for i in 0..109 {
            let v = if i < f { (d + i) as u32 } else { 0xFFFF_FFFF };
            put32(&mut b, 0x4C + 4 * i, v);
        }
        let sec = |s: usize| 512 + s * 512;
        for i in 0..d + f {
            let v = if i + 1 < d {
                i as u32 + 1
            } else if i + 1 == d {
                0xFFFF_FFFE
            } else {
                0xFFFF_FFFD
            };
            put32(&mut b, sec(d + i / 128) + 4 * (i % 128), v);
        }
        let entries = d * 4;
        for id in 0..entries {
            let e = sec(0) + id * 128;
            put16(&mut b, e, 0x41);
            put16(&mut b, e + 0x40, 4);
            b[e + 0x42] = if id == 0 { 5 } else { 2 };
            let right = if id == 0 || id == entries - 1 {
                0xFFFF_FFFF
            } else {
                id as u32 + 1
            };
            put32(&mut b, e + 0x44, right);
            put32(&mut b, e + 0x48, 0xFFFF_FFFF);
            put32(&mut b, e + 0x4C, if id == 0 { 1 } else { 0xFFFF_FFFF });
            put32(&mut b, e + 0x74, 0);
            put32(&mut b, e + 0x78, if id == 0 { 0 } else { (d * 512) as u32 });
        }
        b
    }

    #[test]
    fn streams_sharing_one_chain_stay_within_four_times_the_file() {
        let b = shared_chain_file(500);
        let start = std::time::Instant::now();
        // Rejected (over budget), or kept small: never 2,000 copies.
        if let Some(c) = Container::parse(&b) {
            let total: usize = c.nodes.iter().map(|n| n.data.len()).sum();
            assert!(total <= 4 * b.len(), "{total} bytes from {}", b.len());
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn rebuilding_twice_gives_the_same_bytes() {
        let bytes = data("crates/carrysheet-vba/tests/data/SimpleMacro.xls");
        let c = Container::parse(&bytes).unwrap();
        let a = c.build().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(a, c.build().unwrap());
    }

    #[test]
    fn not_a_compound_file() {
        assert_eq!(Container::parse(b"a,b\r\n"), None);
        assert_eq!(Container::parse(&[0u8; 4096]), None);
    }
}
