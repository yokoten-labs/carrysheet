//! Shared helpers: test data, compound files and a BIFF8 structure check.
#![allow(dead_code)]

use carrysheet_cfb::{Builder, CompoundFile, Version};

pub fn data(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Reads one stream out of a compound file.
pub fn stream(file: &[u8], path: &str) -> Vec<u8> {
    CompoundFile::open(file).unwrap().read_stream(path).unwrap()
}

/// The `Workbook` stream of an `.xls` test file.
pub fn workbook(name: &str) -> Vec<u8> {
    stream(&data(name), "/Workbook")
}

/// Builds a compound file from (path, bytes); storages are made as needed.
pub fn compound(streams: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cf = Builder::new(Version::V3);
    for (path, bytes) in streams {
        let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
        cf.create_storage_all(parent).unwrap();
        cf.put_stream(path, bytes.to_vec()).unwrap();
    }
    cf.to_bytes().unwrap()
}

/// Replaces one stream in a compound file.
pub fn replace_stream(file: &[u8], path: &str, bytes: &[u8]) -> Vec<u8> {
    let mut cf = Builder::from_file(&CompoundFile::open(file).unwrap()).unwrap();
    cf.put_stream(path, bytes.to_vec()).unwrap();
    cf.to_bytes().unwrap()
}

/// UTF-16LE bytes of a string.
pub fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// One BIFF record: (offset, type, body).
pub fn records(stream: &[u8]) -> Vec<(usize, u16, &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 4 <= stream.len() {
        let ty = u16::from_le_bytes([stream[pos], stream[pos + 1]]);
        let len = usize::from(u16::from_le_bytes([stream[pos + 2], stream[pos + 3]]));
        assert!(
            pos + 4 + len <= stream.len(),
            "record at {pos} runs past the end"
        );
        out.push((pos, ty, &stream[pos + 4..pos + 4 + len]));
        pos += 4 + len;
    }
    out
}

/// Checks that a plain (or encrypted: these fields stay readable only in
/// plain streams, so pass `plain = false` to check BOUNDSHEET alone) BIFF8
/// stream has consistent stream positions: every BOUNDSHEET points at a
/// BOF, every INDEX entry at a DBCELL, every EXTSST entry inside the SST.
pub fn check_positions(stream: &[u8], plain: bool) {
    let recs = records(stream);
    let at = |pos: u32| recs.iter().find(|r| r.0 == pos as usize).map(|r| r.1);
    let mut sheets = 0;
    for (off, ty, body) in &recs {
        match *ty {
            0x0085 => {
                let pos = u32::from_le_bytes(body[..4].try_into().unwrap());
                assert_eq!(at(pos), Some(0x0809), "BOUNDSHEET at {off} points at {pos}");
                sheets += 1;
            }
            0x020B if plain => {
                for c in body[16..].as_chunks::<4>().0 {
                    let pos = u32::from_le_bytes(*c);
                    assert_eq!(at(pos), Some(0x00D7), "INDEX at {off} points at {pos}");
                }
            }
            0x00FF if plain => {
                let sst = recs.iter().find(|r| r.1 == 0x00FC).unwrap().0;
                for c in body[2..].as_chunks::<8>().0 {
                    let pos = u32::from_le_bytes(c[..4].try_into().unwrap()) as usize;
                    assert!(pos > sst && pos < *off, "EXTSST at {off} points at {pos}");
                }
            }
            _ => {}
        }
    }
    assert!(sheets > 0, "no BOUNDSHEET records");
}
