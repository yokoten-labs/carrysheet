//! Review round 1: cost amplification and data a copy cannot keep. Each
//! file is built in code.

mod common;

use std::time::{Duration, Instant};

use carrysheet_cfb::{Builder, CfbError, CompoundFile, EntryKind, LossKind, Meta, NodeId, Version};
use common::{END, Raw, data, entry_id, entry_offset, fat_offset, raw_v4, set32, shared_chain};

/// Debug builds are about ten times slower than release; the bounds here
/// hold for either.
const SLOW: Duration = Duration::from_secs(5);

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    assert!(start.elapsed() < SLOW, "{what} took {:?}", start.elapsed());
    out
}

// ---- H2: adding entries costs the same however wide a storage is ----

#[test]
fn a_wide_storage_builds_quickly() {
    let n = 19_990;
    let bytes = timed("building 19,990 entries", || {
        let mut b = Builder::new(Version::V3);
        let s = b.node("/").unwrap();
        let vba = b
            .create_child(
                s,
                &units("_VBA_PROJECT_CUR"),
                EntryKind::Storage,
                Meta::default(),
                vec![],
            )
            .unwrap();
        for i in 0..n {
            let name = units(&format!("{}{i:08}", "M".repeat(23)));
            b.create_child(vba, &name, EntryKind::Stream, Meta::default(), vec![])
                .unwrap();
        }
        // The path API uses the same index.
        b.create_stream("/_VBA_PROJECT_CUR/last", vec![1]).unwrap();
        assert!(matches!(
            b.create_stream(
                &format!("/_vba_project_cur/{}00000007", "m".repeat(23)),
                vec![]
            ),
            Err(CfbError::AlreadyExists { .. })
        ));
        b.to_bytes().unwrap()
    });
    let cf = CompoundFile::open(&bytes).unwrap();
    assert_eq!(cf.walk("/_VBA_PROJECT_CUR").unwrap().len(), n + 2);
}

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

// ---- H3: the fuzz target's own crashes ----

#[test]
fn fuzz_target_on_review_crash_inputs() {
    let harness = shared_chain(100);
    assert_eq!(harness.len(), 421_888);
    // The two inputs the review's fuzzer found: one word changed each.
    let mut crash1 = harness.clone();
    set32(&mut crash1, 168, 2);
    let mut crash2 = harness.clone();
    set32(&mut crash2, 131_100, 3);
    for (name, bytes) in [("harness", harness), ("crash1", crash1), ("crash2", crash2)] {
        timed(name, || carrysheet_cfb::fuzz_targets::read_write(&bytes));
    }
}

#[test]
fn fuzz_target_on_many_entries_is_linear() {
    // Many detached streams on one chain, each declaring a huge size.
    let n = 20_000;
    let mut es = vec![Raw::new("Root Entry", 5)];
    for i in 0..n {
        let mut s = Raw::new(&format!("d{i}"), 2);
        s.size = 1 << 40;
        es.push(s);
    }
    let (_, d0, _) = raw_v4(&es, 0, &|_| END);
    for e in es.iter_mut().skip(1) {
        e.start = d0;
    }
    let bytes = raw_v4(&es, 0, &|_| END).0;
    timed("fuzz target, detached", || {
        carrysheet_cfb::fuzz_targets::read_write(&bytes)
    });
    // The copy stops at the budget instead of reading the chain n times.
    let cf = CompoundFile::open(&bytes).unwrap();
    let r = timed("copy, detached", || Builder::from_file(&cf));
    assert!(matches!(r, Err(CfbError::LimitExceeded { .. })), "{r:?}");

    // The same with the entries linked in the tree as an unknown type.
    let mut es = vec![Raw::new("Root Entry", 5)];
    es[0].child = 1;
    for i in 0..n {
        let mut s = Raw::new(&format!("u{i}"), 3);
        s.size = 1 << 40;
        if i + 1 < n {
            s.right = (i + 2) as u32;
        }
        es.push(s);
    }
    let (_, d0, _) = raw_v4(&es, 0, &|_| END);
    for e in es.iter_mut().skip(1) {
        e.start = d0;
    }
    let bytes = raw_v4(&es, 0, &|_| END).0;
    timed("fuzz target, unknown", || {
        carrysheet_cfb::fuzz_targets::read_write(&bytes)
    });
}

// ---- M1: failed reads cost budget ----

#[test]
fn failing_reads_use_up_the_budget() {
    // A stream whose chain ends early: each strict read fails, and each
    // failure is charged, so repeating it soon reaches the limit.
    let mut b = common::sample(Version::V3);
    let start = common::le32(&b, entry_offset(&b, entry_id(&b, "Big")) + 116);
    let at = fat_offset(&b, start);
    set32(&mut b, at, END);
    let cf = CompoundFile::open(&b).unwrap();
    let big = cf.entry("/Big").unwrap();
    let mut tries = 0;
    let last = loop {
        tries += 1;
        match cf.read(big) {
            Err(CfbError::Damaged { .. }) => continue,
            other => break other,
        }
    };
    assert!(
        matches!(last, Err(CfbError::LimitExceeded { .. })),
        "{last:?}"
    );
    // 4 times the file, 10,000 bytes a try.
    assert!(tries <= 4 * b.len() / 10_000 + 1, "{tries} tries");
}

// ---- M1, L3: a copy keeps what it can and says what it lost ----

#[test]
fn a_short_stream_is_kept_as_far_as_it_goes_and_reported() {
    let mut b = common::sample(Version::V3);
    let start = common::le32(&b, entry_offset(&b, entry_id(&b, "Big")) + 116);
    let at = fat_offset(&b, start + 3);
    set32(&mut b, at, END);
    let cf = CompoundFile::open(&b).unwrap();
    let copy = Builder::from_file(&cf).unwrap();
    let losses = copy.losses();
    assert_eq!(losses.len(), 1, "{losses:?}");
    assert_eq!(losses[0].name, "Big");
    assert_eq!((losses[0].declared, losses[0].kept), (10_000, 2048));
    assert!(losses[0].why.contains("sector chain holds 2048"));
    assert_eq!(copy.stream("/Big").unwrap(), &data(1, 10_000)[..2048]);
}

#[test]
fn an_unreadable_unknown_or_detached_entry_is_reported() {
    let mut b = Builder::new(Version::V3);
    b.create_stream("/odd", data(4, 5000)).unwrap();
    b.create_stream("/lost", data(5, 5000)).unwrap();
    let mut bytes = b.to_bytes().unwrap();
    let odd = entry_offset(&bytes, entry_id(&bytes, "odd"));
    bytes[odd + 66] = 3;
    let odd_start = common::le32(&bytes, odd + 116);
    // Unlink "lost" (the root's tree is lost <- odd... by name order:
    // "odd" (3) sorts before "lost" (4)); make the root hold "odd" alone.
    let odd_id = entry_id(&bytes, "odd") as u32;
    let root = entry_offset(&bytes, 0);
    set32(&mut bytes, root + 76, odd_id);
    set32(&mut bytes, odd + 68, u32::MAX);
    set32(&mut bytes, odd + 72, u32::MAX);
    // Both chains loop.
    let at = fat_offset(&bytes, odd_start + 1);
    set32(&mut bytes, at, odd_start);
    let lost_start = common::le32(&bytes, entry_offset(&bytes, entry_id(&bytes, "lost")) + 116);
    let at = fat_offset(&bytes, lost_start + 2);
    set32(&mut bytes, at, lost_start);
    let cf = CompoundFile::open(&bytes).unwrap();
    assert_eq!(cf.detached().count(), 1);
    let copy = Builder::from_file(&cf).unwrap();
    let bytes_lost: Vec<_> = copy
        .losses()
        .iter()
        .filter(|l| l.kind == LossKind::StreamBytes)
        .collect();
    let mut names: Vec<_> = bytes_lost.iter().map(|l| l.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["lost", "odd"]);
    assert!(bytes_lost.iter().all(|l| l.why.contains("loops")));
    // "lost" was the top of the root's tree, so it still links to "odd";
    // detached, it keeps no links, and the copy says so.
    let links: Vec<_> = copy
        .losses()
        .iter()
        .filter(|l| l.kind == LossKind::DetachedLinks)
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(links, ["lost"]);
}

#[test]
fn extenxls_copies_with_its_short_package_reported() {
    let path = format!(
        "{}/../carrysheet-crypto/tests/data/xlsx/extenxls_pwd123.xlsx",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(path).unwrap();
    let cf = CompoundFile::open(&bytes).unwrap();
    let copy = Builder::from_file(&cf).unwrap();
    let l = copy.losses();
    assert_eq!(l.len(), 1, "{l:?}");
    assert_eq!(l[0].name, "EncryptedPackage");
    assert_eq!((l[0].declared, l[0].kept), (7848, 6144));
}

// ---- L3: names are copied exactly ----

#[test]
fn names_with_slashes_and_lone_surrogates_survive() {
    let slash = units("a/b");
    let lone = vec![0x0041, 0xD800, 0x0042];
    let mut b = Builder::new(Version::V3);
    let s = b
        .create_child(
            NodeId::ROOT,
            &slash,
            EntryKind::Storage,
            Meta::default(),
            vec![],
        )
        .unwrap();
    b.create_child(s, &lone, EntryKind::Stream, Meta::default(), vec![7; 10])
        .unwrap();
    assert!(matches!(
        b.create_child(s, &lone, EntryKind::Stream, Meta::default(), vec![]),
        Err(CfbError::AlreadyExists { .. })
    ));
    assert!(matches!(
        b.create_child(
            NodeId::ROOT,
            &[0x41; 32],
            EntryKind::Stream,
            Meta::default(),
            vec![]
        ),
        Err(CfbError::InvalidName { .. })
    ));
    let bytes = b.to_bytes().unwrap();
    let once = Builder::from_file(&CompoundFile::open(&bytes).unwrap()).unwrap();
    let twice = once.to_bytes().unwrap();
    assert_eq!(twice, bytes);
    let cf = CompoundFile::open(&twice).unwrap();
    let walk = cf.walk("/").unwrap();
    let names: Vec<&[u16]> = walk.entries().map(|e| e.name_utf16()).collect();
    assert_eq!(names[1], &slash[..]);
    assert_eq!(names[2], &lone[..]);
    assert_eq!(cf.read(walk.items()[2].entry).unwrap(), vec![7; 10]);
}
