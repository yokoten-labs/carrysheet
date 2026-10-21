//! Review round 1, H1: walking deep storages holds parent links, not
//! paths. Alone in its own test binary, so the process's peak memory is
//! this test's.

mod common;

use carrysheet_cfb::{Builder, CompoundFile, EntryKind, Meta, NodeId, Version};

/// Peak resident memory of this process in MB (Linux only).
fn peak_mb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: u64 = s
        .lines()
        .find(|l| l.starts_with("VmHWM"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb / 1024)
}

#[test]
fn a_deep_walk_stays_small() {
    // 61 storages deep, then 99,000 streams, every name 31 code units:
    // full paths would be about 580 MB.
    let long = |i: usize| -> Vec<u16> {
        let mut u: Vec<u16> = std::iter::repeat_n('名' as u16, 23).collect();
        u.extend(format!("{i:08}").encode_utf16());
        u
    };
    let mut b = Builder::new(Version::V4);
    let mut at = b
        .create_child(
            NodeId::ROOT,
            &"_VBA_PROJECT_CUR".encode_utf16().collect::<Vec<_>>(),
            EntryKind::Storage,
            Meta::default(),
            vec![],
        )
        .unwrap();
    for d in 0..61 {
        at = b
            .create_child(at, &long(d), EntryKind::Storage, Meta::default(), vec![])
            .unwrap();
    }
    let n = 99_000;
    for i in 0..n {
        b.create_child(at, &long(i), EntryKind::Stream, Meta::default(), vec![])
            .unwrap();
    }
    let bytes = b.to_bytes().unwrap();
    drop(b);
    let before = peak_mb();
    let cf = CompoundFile::open(&bytes).unwrap();
    let walk = cf.walk("/").unwrap();
    assert_eq!(walk.len(), 1 + 1 + 61 + n);
    assert_eq!(walk.items().last().unwrap().depth, 63);
    // One path on demand is fine.
    assert_eq!(
        walk.path(walk.len() - 1).unwrap().chars().count(),
        62 * 32 + 31 - 15
    );
    if let (Some(before), Some(after)) = (before, peak_mb()) {
        assert!(
            after < before + 100,
            "peak went from {before} MB to {after} MB"
        );
    }
}
