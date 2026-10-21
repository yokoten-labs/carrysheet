//! Hostile files built in code, one per budget: each must fail with an
//! error that names the problem, quickly, without a large allocation.

mod common;

use std::time::{Duration, Instant};

use carrysheet_cfb::{Builder, CfbError, CompoundFile, Limits, Version};
use common::{
    data, entry_id, entry_offset, fat_offset, le32, minifat_offset, sample, sector_len, set32,
    set64,
};

const QUICK: Duration = Duration::from_secs(2);

/// Opens and reads every stream; returns the first error.
fn first_error(bytes: &[u8], limits: Limits) -> CfbError {
    let start = Instant::now();
    let result = (|| {
        let cf = CompoundFile::open_with(bytes, limits)?;
        for e in cf.walk("/")?.entries() {
            if e.is_stream() {
                cf.read(e)?;
            }
        }
        Ok::<_, CfbError>(())
    })();
    assert!(start.elapsed() < QUICK, "took {:?}", start.elapsed());
    let e = result.expect_err("the hostile file was accepted");
    assert!(!e.what().is_empty() && !e.why().is_empty() && !e.fix().is_empty());
    e
}

fn assert_damaged(e: &CfbError, part: &str, why: &str) {
    match e {
        CfbError::Damaged { part: p, detail } => {
            assert!(p.contains(part), "part {p:?}, wanted {part:?}: {e}");
            assert!(detail.contains(why), "{e}");
        }
        other => panic!("expected damage in {part}, got {other:?}"),
    }
}

fn start_of(bytes: &[u8], name: &str) -> u32 {
    le32(bytes, entry_offset(bytes, entry_id(bytes, name)) + 116)
}

// ---- sector chains (FAT) ----

#[test]
fn fat_chain_that_loops() {
    for v in [Version::V3, Version::V4] {
        let mut b = sample(v);
        let s = start_of(&b, "Big");
        // The second sector of "Big" points back to the first.
        let at = fat_offset(&b, s + 1);
        set32(&mut b, at, s);
        let e = first_error(&b, Limits::default());
        assert_damaged(&e, "Big", "loops back to sector");
    }
}

#[test]
fn fat_chain_past_the_end_or_into_a_mark() {
    let mut b = sample(Version::V3);
    let s = start_of(&b, "Big");
    let at = fat_offset(&b, s);
    set32(&mut b, at, 1_000_000);
    assert_damaged(&first_error(&b, Limits::default()), "Big", "only");
    set32(&mut b, at, 0xFFFF_FFFF);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "Big",
        "free-sector mark",
    );
    // A chain that ends early is short, and read_lenient keeps what is there.
    set32(&mut b, at, 0xFFFF_FFFE);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "Big",
        "chain ends after 512",
    );
    let cf = CompoundFile::open(&b).unwrap();
    let got = cf.read_lenient(cf.entry("/Big").unwrap()).unwrap();
    assert_eq!(got, data(1, 512));
}

// ---- sector chains (mini FAT) ----

#[test]
fn mini_fat_chain_that_loops() {
    let mut b = sample(Version::V3);
    let m = start_of(&b, "Small");
    let at = minifat_offset(&b, m + 2);
    set32(&mut b, at, m);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "Small",
        "loops back to mini sector",
    );
    // Big streams do not need the mini FAT and still read.
    let cf = CompoundFile::open(&b).unwrap();
    assert_eq!(cf.read_stream("/Big").unwrap(), data(1, 10_000));
}

#[test]
fn mini_fat_chain_past_the_end() {
    let mut b = sample(Version::V3);
    let m = start_of(&b, "Small");
    let at = minifat_offset(&b, m);
    set32(&mut b, at, 50_000);
    assert_damaged(&first_error(&b, Limits::default()), "Small", "mini sector");
}

#[test]
fn mini_fat_chain_that_loops_in_the_header() {
    // The mini FAT's own chain loops: small streams fail, the file opens.
    let mut b = sample(Version::V3);
    let s = le32(&b, 60);
    let at = fat_offset(&b, s);
    set32(&mut b, at, s);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "mini FAT",
        "loops back",
    );
    assert!(CompoundFile::open(&b).is_ok());
}

// ---- DIFAT ----

/// A file with DIFAT sectors: over 109 FAT sectors of 512 bytes.
fn with_difat() -> Vec<u8> {
    let mut b = Builder::new(Version::V3);
    b.create_stream("/Big", vec![0x5A; 7_200_000]).unwrap();
    let bytes = b.to_bytes().unwrap();
    assert_eq!(le32(&bytes, 72), 1, "one DIFAT sector");
    bytes
}

#[test]
fn difat_that_loops() {
    let mut b = with_difat();
    let ss = sector_len(&b);
    let d = le32(&b, 68);
    // The DIFAT sector's "next" field points to itself.
    let at = (d as usize + 1) * ss + ss - 4;
    set32(&mut b, at, d);
    assert_damaged(&first_error(&b, Limits::default()), "DIFAT", "loops back");
}

#[test]
fn difat_past_the_end() {
    let mut b = with_difat();
    set32(&mut b, 68, 9_999_999);
    assert_damaged(&first_error(&b, Limits::default()), "DIFAT", "only");
}

#[test]
fn difat_that_lists_more_fat_sectors_than_the_file_has() {
    // A tiny file whose DIFAT sector is full of FAT sector numbers.
    let mut b = sample(Version::V3);
    let ss = sector_len(&b);
    // Reuse the last sector of the file as a DIFAT sector.
    let last = (b.len() / ss - 2) as u32;
    set32(&mut b, 68, last);
    set32(&mut b, 72, 1);
    let at = (last as usize + 1) * ss;
    for k in 0..(ss / 4 - 1) {
        set32(&mut b, at + 4 * k, 0);
    }
    for k in 1..109 {
        set32(&mut b, 76 + 4 * k, 0);
    }
    set32(&mut b, at + ss - 4, 0xFFFF_FFFE);
    set32(&mut b, 44, 109 + 127);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "DIFAT",
        "but the whole file has only",
    );
}

// ---- directory ----

#[test]
fn directory_entry_count_is_capped() {
    let mut b = Builder::new(Version::V3);
    for i in 0..50 {
        b.create_stream(&format!("/s{i}"), vec![1]).unwrap();
    }
    let bytes = b.to_bytes().unwrap();
    let limits = Limits {
        max_entries: 20,
        ..Limits::default()
    };
    match first_error(&bytes, limits) {
        CfbError::LimitExceeded { what, limit } => {
            assert_eq!((what, limit), ("directory entries", 20));
        }
        other => panic!("{other:?}"),
    }
    assert!(CompoundFile::open(&bytes).is_ok());
}

#[test]
fn directory_chain_that_loops() {
    let mut b = Builder::new(Version::V3);
    for i in 0..20 {
        b.create_stream(&format!("/s{i}"), vec![1]).unwrap();
    }
    let mut bytes = b.to_bytes().unwrap();
    let d = le32(&bytes, 48);
    let at = fat_offset(&bytes, d + 2);
    set32(&mut bytes, at, d);
    assert_damaged(
        &first_error(&bytes, Limits::default()),
        "directory",
        "loops back",
    );
}

#[test]
fn tree_link_that_loops() {
    let mut b = sample(Version::V3);
    // "S" (one letter) sorts first in the root's tree; point its right
    // link back at the top of the tree.
    let root_child = le32(&b, entry_offset(&b, 0) + 76);
    let s = entry_offset(&b, entry_id(&b, "S"));
    set32(&mut b, s + 72, root_child);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "directory",
        "linked twice",
    );
    // A storage that holds itself.
    let mut b = sample(Version::V3);
    let id = entry_id(&b, "S") as u32;
    let s = entry_offset(&b, id as usize);
    set32(&mut b, s + 76, id);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "directory",
        "linked twice",
    );
}

#[test]
fn tree_link_to_nothing() {
    let mut b = sample(Version::V3);
    let s = entry_offset(&b, entry_id(&b, "S"));
    set32(&mut b, s + 68, 4000);
    assert_damaged(&first_error(&b, Limits::default()), "directory", "only");
    // A link to the unused slot at the end of the directory sector.
    let mut b = sample(Version::V3);
    let s = entry_offset(&b, entry_id(&b, "S"));
    set32(&mut b, s + 68, 7);
    assert_damaged(&first_error(&b, Limits::default()), "directory", "unused");
}

#[test]
fn tree_depth_is_capped() {
    let mut b = Builder::new(Version::V3);
    let mut path = String::new();
    for i in 0..70 {
        path.push_str(&format!("/d{i}"));
        b.create_storage(&path).unwrap();
    }
    b.create_stream(&format!("{path}/leaf"), vec![1]).unwrap();
    let bytes = b.to_bytes().unwrap();
    match first_error(&bytes, Limits::default()) {
        CfbError::LimitExceeded { what, limit } => {
            assert_eq!((what, limit), ("levels of nested storages", 64));
        }
        other => panic!("{other:?}"),
    }
    let deep = Limits {
        max_depth: 100,
        ..Limits::default()
    };
    let cf = CompoundFile::open_with(&bytes, deep).unwrap();
    assert!(cf.is_stream(&format!("{path}/leaf")));
}

#[test]
fn a_long_degenerate_tree_reads() {
    // Some writers chain siblings as a list instead of a balanced tree:
    // allowed, and walked without recursion.
    let mut b = Builder::new(Version::V3);
    let n = 3000;
    for i in 0..n {
        b.create_stream(&format!("/s{i:04}"), vec![i as u8])
            .unwrap();
    }
    let mut bytes = b.to_bytes().unwrap();
    // Relink: root -> 1 -> 2 -> ... by right links only.
    let root = entry_offset(&bytes, 0);
    set32(&mut bytes, root + 76, 1);
    for id in 1..=n {
        let at = entry_offset(&bytes, id);
        set32(&mut bytes, at + 68, u32::MAX);
        let next = if id < n { id as u32 + 1 } else { u32::MAX };
        set32(&mut bytes, at + 72, next);
    }
    let cf = CompoundFile::open(&bytes).unwrap();
    assert_eq!(cf.children(cf.root()).count(), n);
    assert_eq!(cf.read_stream("/s2999").unwrap(), [(2999 % 256) as u8]);
}

#[test]
fn bad_name_lengths_and_roots() {
    let mut b = sample(Version::V3);
    let s = entry_offset(&b, entry_id(&b, "S"));
    b[s + 64] = 99;
    assert_damaged(&first_error(&b, Limits::default()), "directory", "name");
    let mut b = sample(Version::V3);
    let root = entry_offset(&b, 0);
    b[root + 66] = 1;
    assert_damaged(&first_error(&b, Limits::default()), "directory", "root");
    let mut b = sample(Version::V3);
    let s = entry_offset(&b, entry_id(&b, "S"));
    b[s + 66] = 5;
    assert_damaged(
        &first_error(&b, Limits::default()),
        "directory",
        "second root",
    );
}

// ---- mini stream size ----

#[test]
fn mini_stream_larger_than_the_file() {
    let mut b = sample(Version::V3);
    let root = entry_offset(&b, 0);
    set64(&mut b, root + 120, 0xFFFF_FFFF);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "mini stream",
        "whole file",
    );
    // The file still opens, and big streams read.
    let cf = CompoundFile::open(&b).unwrap();
    assert_eq!(cf.read_stream("/Big").unwrap(), data(1, 10_000));
}

#[test]
fn mini_sector_past_the_mini_stream() {
    let mut b = sample(Version::V3);
    let root = entry_offset(&b, 0);
    // The mini stream says it is one mini sector long.
    set64(&mut b, root + 120, 64);
    assert_damaged(
        &first_error(&b, Limits::default()),
        "stream",
        "past the end of the mini stream",
    );
}

// ---- stream size against the file size ----

#[test]
fn stream_larger_than_the_file() {
    for v in [Version::V3, Version::V4] {
        let mut b = sample(v);
        let big = entry_offset(&b, entry_id(&b, "Big"));
        let huge = if v == Version::V3 {
            0xFFFF_0000
        } else {
            1 << 50
        };
        set64(&mut b, big + 120, huge);
        assert_damaged(
            &first_error(&b, Limits::default()),
            "Big",
            "but the whole file is",
        );
        // read_lenient returns what the chain holds, no more.
        let cf = CompoundFile::open(&b).unwrap();
        let got = cf.read_lenient(cf.entry("/Big").unwrap()).unwrap();
        assert!(got.len() < b.len());
        assert_eq!(got[..10_000], data(1, 10_000)[..]);
    }
}

#[test]
fn version_3_ignores_the_high_size_bits() {
    let mut b = sample(Version::V3);
    let small = entry_offset(&b, entry_id(&b, "Small"));
    b[small + 124..small + 128].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let cf = CompoundFile::open(&b).unwrap();
    assert_eq!(cf.read_stream("/Small").unwrap(), data(2, 300));
}

// ---- total bytes read ----

#[test]
fn total_bytes_read_is_capped() {
    // 200 entries share one big chain: each read is allowed, all of them
    // together would produce 200 times the file.
    let mut b = Builder::new(Version::V3);
    b.create_stream("/Big", data(1, 20_000)).unwrap();
    for i in 0..200 {
        b.create_stream(&format!("/c{i:03}"), vec![]).unwrap();
    }
    let mut bytes = b.to_bytes().unwrap();
    let start = start_of(&bytes, "Big");
    for i in 0..200 {
        let at = entry_offset(&bytes, entry_id(&bytes, &format!("c{i:03}")));
        set32(&mut bytes, at + 116, start);
        set64(&mut bytes, at + 120, 20_000);
    }
    match first_error(&bytes, Limits::default()) {
        CfbError::LimitExceeded { what, limit } => {
            assert_eq!(what, "bytes of stream data");
            assert_eq!(limit, 4 * bytes.len() as u64);
        }
        other => panic!("{other:?}"),
    }
    // Reading each stream once, from a fresh open, is fine.
    let cf = CompoundFile::open(&bytes).unwrap();
    assert_eq!(cf.read_stream("/c007").unwrap(), data(1, 20_000));
    // A copy needs every stream, so it is refused as well.
    let cf = CompoundFile::open(&bytes).unwrap();
    assert!(matches!(
        Builder::from_file(&cf),
        Err(CfbError::LimitExceeded { .. })
    ));
}

#[test]
fn output_ratio_can_be_tightened() {
    let bytes = sample(Version::V3);
    let limits = Limits {
        max_output_ratio: 0,
        ..Limits::default()
    };
    let cf = CompoundFile::open_with(&bytes, limits).unwrap();
    assert!(matches!(
        cf.read_stream("/Big"),
        Err(CfbError::LimitExceeded { .. })
    ));
}

// ---- header and cut files ----

#[test]
fn header_values_are_checked() {
    let good = sample(Version::V3);
    for (at, value, hint) in [
        (28usize, 0xFEFFu16, "byte order"),
        (26, 2, "major version"),
        (30, 12, "512-byte sectors"),
        (32, 7, "mini sector shift"),
    ] {
        let mut b = good.clone();
        b[at..at + 2].copy_from_slice(&value.to_le_bytes());
        match first_error(&b, Limits::default()) {
            CfbError::Unsupported { detail } => assert!(detail.contains(hint), "{detail}"),
            other => panic!("{other:?}"),
        }
    }
    let mut b = good.clone();
    set32(&mut b, 56, 512);
    assert!(matches!(
        first_error(&b, Limits::default()),
        CfbError::Unsupported { .. }
    ));
}

#[test]
fn every_cut_fails_cleanly() {
    for v in [Version::V3, Version::V4] {
        let good = sample(v);
        for cut in (0..good.len()).step_by(37) {
            let _ = first_error_or_ok(&good[..cut]);
        }
    }
}

fn first_error_or_ok(bytes: &[u8]) -> Result<(), CfbError> {
    let cf = CompoundFile::open(bytes)?;
    for e in cf.walk("/")?.entries() {
        if e.is_stream() {
            cf.read(e)?;
        }
    }
    Ok(())
}

// ---- Review round 2: a prefix needs only its own bytes ----

#[test]
fn a_prefix_reads_when_the_size_is_larger_than_the_file() {
    let mut b = Builder::new(Version::V3);
    b.create_stream("/Workbook", common::data(7, 10_000))
        .unwrap();
    let mut bytes = b.to_bytes().unwrap();
    let at = common::entry_offset(&bytes, common::entry_id(&bytes, "Workbook"));
    common::set32(&mut bytes, at + 120, 0x4000_0000);
    let cf = CompoundFile::open(&bytes).unwrap();
    let e = cf.entry("/Workbook").unwrap();
    assert!(e.size() > bytes.len() as u64);
    // The bytes asked for are there.
    assert_eq!(
        cf.read_prefix(e, 5000).unwrap(),
        common::data(7, 10_000)[..5000]
    );
    // More than the chain holds, or the whole stream, is still an error.
    let short = cf.read_prefix(e, 20_000).unwrap_err();
    assert!(matches!(short, CfbError::Damaged { .. }), "{short}");
    let whole = cf.read(e).unwrap_err();
    assert!(whole.why().contains("whole file"), "{whole}");
    // Asking for more than the whole file is refused before any chain.
    let huge = cf.read_prefix(e, 1 << 20).unwrap_err();
    assert!(huge.why().contains("whole file"), "{huge}");
}
