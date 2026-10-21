//! Review round 2: what a copy keeps as it was (notes) and what it cannot
//! keep (losses), beyond stream bytes.

mod common;

use carrysheet_cfb::{Builder, CompoundFile, EntryKind, LossKind, Meta, NodeId, Version};
use common::{Raw, data, entry_id, entry_offset, raw_v4, set32};

/// Rewrites entry `name` so its stored name has no terminating null:
/// `extra` more units are written over where the null was, and the
/// length counts them without a null.
fn unterminate(bytes: &mut [u8], name: &str, extra: &[u16]) -> usize {
    let at = entry_offset(bytes, entry_id(bytes, name));
    let n = name.encode_utf16().count();
    for (k, u) in extra.iter().enumerate() {
        let p = at + 2 * (n + k);
        bytes[p..p + 2].copy_from_slice(&u.to_le_bytes());
    }
    let len = ((n + extra.len()) * 2) as u16;
    bytes[at + 64..at + 66].copy_from_slice(&len.to_le_bytes());
    at
}

// ---- L2: a name without its terminating null ----

#[test]
fn a_name_without_its_null_keeps_every_unit() {
    let mut b = Builder::new(Version::V3);
    b.create_stream("/Abc", data(1, 300)).unwrap();
    let mut bytes = b.to_bytes().unwrap();
    // The length says 3 units and none of them is null: the old reader
    // took the last one for the null and read "Ab".
    unterminate(&mut bytes, "Abc", &[]);
    let at = entry_offset(&bytes, entry_id(&bytes, "Abc"));
    bytes[at + 64..at + 66].copy_from_slice(&6u16.to_le_bytes());

    let cf = CompoundFile::open(&bytes).unwrap();
    let e = cf.entry("/Abc").expect("found by its whole name");
    assert_eq!(e.name(), "Abc");
    assert!(!e.name_terminated());
    assert!(cf.entry("/Ab").is_none());
    assert_eq!(cf.read(e).unwrap(), data(1, 300));

    // A copy keeps it exactly, and says so.
    let copy = Builder::from_file(&cf).unwrap();
    assert!(copy.losses().is_empty(), "{:?}", copy.losses());
    let notes = copy.notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].name, "Abc");
    assert!(notes[0].why.contains("no terminating null"), "{notes:?}");
    let written = copy.to_bytes().unwrap();
    let again = CompoundFile::open(&written).unwrap();
    let e2 = again.entry("/Abc").unwrap();
    assert!(!e2.name_terminated());
    assert_eq!(e2.name_utf16(), e.name_utf16());
    assert_eq!(again.read(e2).unwrap(), data(1, 300));
    carrysheet_cfb::fuzz_targets::read_write(&bytes);
}

#[test]
fn a_32_unit_name_without_a_null_round_trips() {
    let name = "N".repeat(31);
    let mut b = Builder::new(Version::V4);
    b.create_stream(&format!("/{name}"), data(2, 5000)).unwrap();
    let mut bytes = b.to_bytes().unwrap();
    unterminate(&mut bytes, &name, &[u16::from(b'X')]);

    let cf = CompoundFile::open(&bytes).unwrap();
    let full = format!("{name}X");
    let e = cf.entry(&format!("/{full}")).unwrap();
    assert_eq!(e.name_utf16().len(), 32);
    assert!(!e.name_terminated());

    let copy = Builder::from_file(&cf).unwrap();
    assert!(copy.losses().is_empty(), "{:?}", copy.losses());
    assert_eq!(copy.notes().len(), 1);
    let written = copy.to_bytes().unwrap();
    let again = CompoundFile::open(&written).unwrap();
    let e2 = again.entry(&format!("/{full}")).unwrap();
    assert_eq!(e2.name(), full);
    assert_eq!(again.read(e2).unwrap(), data(2, 5000));
    carrysheet_cfb::fuzz_targets::read_write(&bytes);
}

#[test]
fn a_terminated_name_gives_no_note() {
    let bytes = common::sample(Version::V3);
    let cf = CompoundFile::open(&bytes).unwrap();
    assert!(cf.walk("/").unwrap().entries().all(|e| e.name_terminated()));
    let copy = Builder::from_file(&cf).unwrap();
    assert!(copy.notes().is_empty());
    assert!(copy.losses().is_empty());
}

#[test]
fn a_new_name_cannot_take_32_units() {
    let mut b = Builder::new(Version::V3);
    assert!(
        b.create_stream(&format!("/{}", "N".repeat(32)), vec![])
            .is_err()
    );
}

// ---- L3: structure a copy cannot keep ----

/// Root -> stream S, whose child link reaches storage T (with a start and
/// size), whose child is stream U.
fn stream_storage_stream() -> Vec<u8> {
    let mut root = Raw::new("Root Entry", 5);
    root.child = 1;
    let mut s = Raw::new("S", 2);
    s.child = 2;
    let mut t = Raw::new("T", 1);
    t.child = 3;
    t.start = 7;
    t.size = 99;
    let u = Raw::new("U", 2);
    raw_v4(&[root, s, t, u], 0, &|_| 0).0
}

#[test]
fn a_child_link_on_a_stream_is_reported() {
    let bytes = stream_storage_stream();
    let cf = CompoundFile::open(&bytes).unwrap();
    let detached: Vec<_> = cf.detached().map(|e| e.name().to_string()).collect();
    assert_eq!(detached, ["T", "U"]);

    let copy = Builder::from_file(&cf).unwrap();
    let mut got: Vec<_> = copy
        .losses()
        .iter()
        .map(|l| (l.name.as_str(), l.kind))
        .collect();
    got.sort_by_key(|&(n, _)| n);
    assert_eq!(
        got,
        [("S", LossKind::StreamChild), ("T", LossKind::DetachedLinks)],
        "{:?}",
        copy.losses()
    );
    for l in copy.losses() {
        assert!(l.why.contains("the copy keeps"), "{l:?}");
    }

    // The copy keeps all four entries, and T's start and size as stored;
    // the links are gone, so a copy of the copy loses nothing more.
    let written = copy.to_bytes().unwrap();
    let again = CompoundFile::open(&written).unwrap();
    assert_eq!(again.entry("/S").unwrap().kind(), EntryKind::Stream);
    let mut kept: Vec<_> = again.detached().map(|e| e.name().to_string()).collect();
    kept.sort();
    assert_eq!(kept, ["T", "U"]);
    let t = entry_offset(&written, entry_id(&written, "T"));
    assert_eq!(common::le32(&written, t + 116), 7);
    assert_eq!(common::le32(&written, t + 120), 99);
    let copy2 = Builder::from_file(&again).unwrap();
    assert!(copy2.losses().is_empty(), "{:?}", copy2.losses());
}

#[test]
fn a_storage_start_and_size_are_written_back() {
    // Some writers put the end-of-chain mark in a storage's start.
    let mut b = Builder::new(Version::V3);
    b.create_storage("/S").unwrap();
    b.create_stream("/S/x", data(3, 10)).unwrap();
    let mut bytes = b.to_bytes().unwrap();
    let at = entry_offset(&bytes, entry_id(&bytes, "S"));
    assert_eq!(common::le32(&bytes, at + 116), 0, "new storages write 0");
    set32(&mut bytes, at + 116, 0xFFFF_FFFE);
    let cf = CompoundFile::open(&bytes).unwrap();
    assert_eq!(cf.entry("/S").unwrap().size(), 0);
    let copy = Builder::from_file(&cf).unwrap();
    assert!(copy.losses().is_empty(), "{:?}", copy.losses());
    let written = copy.to_bytes().unwrap();
    let at = entry_offset(&written, entry_id(&written, "S"));
    assert_eq!(common::le32(&written, at + 116), 0xFFFF_FFFE);
    // Nothing is lost, but the copy says the file breaks the rule.
    let notes = copy.notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].name, "S");
    assert!(notes[0].why.contains("0xFFFFFFFE"), "{notes:?}");
    carrysheet_cfb::fuzz_targets::read_write(&bytes);
}

// ---- Review round 3 ----

#[test]
fn root_sibling_links_are_reported() {
    let mut bytes = common::sample(Version::V3);
    let small = entry_id(&bytes, "Small") as u32;
    let root = entry_offset(&bytes, 0);
    set32(&mut bytes, root + 72, small);
    let cf = CompoundFile::open(&bytes).unwrap();
    let copy = Builder::from_file(&cf).unwrap();
    let l = copy.losses();
    assert_eq!(l.len(), 1, "{l:?}");
    assert_eq!((l[0].id, l[0].kind), (0, LossKind::DetachedLinks));
    assert!(l[0].why.contains("root"), "{l:?}");
    assert!(l[0].why.contains(&small.to_string()), "{l:?}");
    let written = copy.to_bytes().unwrap();
    let again = CompoundFile::open(&written).unwrap();
    assert!(Builder::from_file(&again).unwrap().losses().is_empty());
}

#[test]
fn a_storage_start_or_size_gets_a_note() {
    // stream -> storage -> stream gives T start 7 and size 99.
    let cf_bytes = stream_storage_stream();
    let cf = CompoundFile::open(&cf_bytes).unwrap();
    let copy = Builder::from_file(&cf).unwrap();
    let notes: Vec<_> = copy.notes().iter().filter(|n| n.name == "T").collect();
    assert_eq!(notes.len(), 1, "{:?}", copy.notes());
    assert!(notes[0].why.contains("size 99"), "{notes:?}");
}

#[test]
fn an_empty_name_stored_with_length_2_gets_a_note() {
    let mut b = Builder::new(Version::V3);
    b.create_child(
        NodeId::ROOT,
        &[],
        EntryKind::Stream,
        Meta::default(),
        data(4, 100),
    )
    .unwrap();
    b.create_stream("/Z", data(5, 100)).unwrap();
    let mut bytes = b.to_bytes().unwrap();
    let cf = CompoundFile::open(&bytes).unwrap();
    let empty = cf
        .walk("/")
        .unwrap()
        .entries()
        .find(|e| e.name().is_empty() && e.is_stream())
        .unwrap()
        .id() as usize;
    drop(cf);
    let at = entry_offset(&bytes, empty);
    assert_eq!(&bytes[at + 64..at + 66], &[0, 0]);
    bytes[at + 64..at + 66].copy_from_slice(&2u16.to_le_bytes());

    let cf = CompoundFile::open(&bytes).unwrap();
    let copy = Builder::from_file(&cf).unwrap();
    assert!(copy.losses().is_empty());
    let n = copy.notes();
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].why.contains("name length is 2 bytes"), "{n:?}");
    let written = copy.to_bytes().unwrap();
    let at = entry_offset(&written, empty);
    assert_eq!(&written[at + 64..at + 66], &[0, 0]);
}

#[test]
fn v3_size_high_bits_get_a_note() {
    let mut bytes = common::sample(Version::V3);
    let at = entry_offset(&bytes, entry_id(&bytes, "Big"));
    set32(&mut bytes, at + 124, 0xDEAD);
    let cf = CompoundFile::open(&bytes).unwrap();
    let e = cf.entry("/Big").unwrap();
    assert_eq!(e.size(), 10_000);
    assert_eq!(cf.read(e).unwrap(), data(1, 10_000));
    let copy = Builder::from_file(&cf).unwrap();
    assert!(copy.losses().is_empty());
    let n = copy.notes();
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].name, "Big");
    assert!(n[0].why.contains("0x0000DEAD"), "{n:?}");
    let written = copy.to_bytes().unwrap();
    let at = entry_offset(&written, entry_id(&written, "Big"));
    assert_eq!(common::le32(&written, at + 124), 0);
    // Version 4 keeps all 64 bits, so no note: the size is the size.
    let ok = common::sample(Version::V4);
    let copy = Builder::from_file(&CompoundFile::open(&ok).unwrap()).unwrap();
    assert!(copy.notes().is_empty());
}
