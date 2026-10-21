//! Reading the fixtures, writing files, and read-then-write round trips.

mod common;

use carrysheet_cfb::{Builder, CfbError, CompoundFile, EntryKind, Meta, Version};
use common::{data, entry_id, entry_offset, fixture, set32, set64, workspace_files};

/// The fixtures' streams: (path, length, data seed), as tests/data/README
/// says they were made.
fn fixture_streams() -> Vec<(String, usize, u32)> {
    let mut all: Vec<(String, usize)> = vec![
        ("Small".into(), 100),
        ("Big".into(), 5000),
        ("Empty".into(), 0),
        ("Sub/Inner".into(), 4095),
        ("Sub/Exact".into(), 4096),
        ("Sub/Deeper/Leaf".into(), 64),
        ("\u{5}SummaryInformation".into(), 200),
        ("Ünïcode 名前".into(), 300),
    ];
    for i in 0..40 {
        all.push((format!("N{i:02}"), 10 + i * 37));
    }
    all.into_iter()
        .enumerate()
        .map(|(i, (p, len))| (p, len, i as u32 + 1))
        .collect()
}

#[test]
fn fixtures_of_both_versions_read() {
    for (name, version) in [("v3.cfb", Version::V3), ("v4.cfb", Version::V4)] {
        let bytes = fixture(name);
        let cf = CompoundFile::open(&bytes).unwrap();
        assert_eq!(cf.version(), version, "{name}");
        for (path, len, seed) in fixture_streams() {
            let e = cf.entry(&path).unwrap_or_else(|| panic!("{name}: {path}"));
            assert!(e.is_stream());
            assert_eq!(e.size(), len as u64, "{name}: {path}");
            assert_eq!(cf.read(e).unwrap(), data(seed, len), "{name}: {path}");
        }
        assert!(cf.is_storage("/Sub/Deeper"));
        assert!(cf.is_storage("/"));
        assert!(!cf.exists("/Nope"));
        // Names compare without case, as in MS-CFB.
        assert_eq!(cf.read_stream("/sub/INNER").unwrap().len(), 4095);
        assert_eq!(cf.entry("/Small").unwrap().state_bits(), 0x1234_5678);
        assert_eq!(cf.entry("/Sub").unwrap().state_bits(), 0x0BAD_F00D);
        // The writer stamped storage times; they are kept raw.
        assert_ne!(cf.entry("/Sub").unwrap().modified(), 0);
        // Walk: parents first, paths relative to the start.
        let walk = cf.walk("/Sub").unwrap();
        let paths: Vec<String> = (0..walk.len()).map(|i| walk.path(i).unwrap()).collect();
        assert_eq!(paths, ["", "Exact", "Inner", "Deeper", "Deeper/Leaf"]);
        let depths: Vec<usize> = walk.items().iter().map(|i| i.depth).collect();
        assert_eq!(depths, [0, 1, 1, 1, 2]);
        assert_eq!(walk.items()[4].parent, Some(3));
        assert_eq!(cf.walk("/").unwrap().len(), 1 + 48 + 2);
        assert_eq!(cf.detached().count(), 0);
        // A prefix.
        let big = cf.entry("/Big").unwrap();
        assert_eq!(cf.read_prefix(big, 10).unwrap(), data(2, 10));
    }
}

/// Everything in a file, independent of its layout. Streams are read
/// leniently, as a copy reads them.
fn content(cf: &CompoundFile<'_>) -> Vec<(String, EntryKind, Meta, Vec<u8>)> {
    let walk = cf.walk("/").unwrap();
    let mut out: Vec<_> = walk
        .items()
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let e = item.entry;
            let bytes = if e.is_stream() {
                // A copy keeps nothing of a stream it cannot read, and
                // reports it.
                cf.read_lenient(e).unwrap_or_default()
            } else {
                Vec::new()
            };
            (walk.path(i).unwrap(), e.kind(), e.meta(), bytes)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn read_then_write_keeps_every_stream_and_value() {
    let mut files = workspace_files();
    files.push(("v3.cfb".into(), fixture("v3.cfb")));
    files.push(("v4.cfb".into(), fixture("v4.cfb")));
    let (mut kept, mut refused) = (0, Vec::new());
    for (name, bytes) in &files {
        let Ok(cf) = CompoundFile::open(bytes) else {
            refused.push(name.clone());
            continue;
        };
        let Ok(copy) = Builder::from_file(&cf) else {
            refused.push(name.clone());
            continue;
        };
        // Only files with a stream cut short lose bytes, and say so.
        if !copy.losses().is_empty() {
            assert!(
                name.ends_with("extenxls_pwd123.xlsx") || name.contains("/fuzz/clusterfuzz-"),
                "{name}: {:?}",
                copy.losses()
            );
        }
        let written = copy.to_bytes().unwrap();
        let again = CompoundFile::open(&written).unwrap();
        assert_eq!(again.version(), cf.version(), "{name}");
        assert_eq!(content(&again), content(&cf), "{name}");
        assert_eq!(again.root().meta(), cf.root().meta(), "{name}");
        // The layout is fixed, so writing the copy again changes nothing.
        let twice = Builder::from_file(&again).unwrap().to_bytes().unwrap();
        assert_eq!(twice, written, "{name}");
        carrysheet_cfb::fuzz_targets::read_write(bytes);
        kept += 1;
    }
    eprintln!("{kept} files kept; refused: {refused:?}");
    assert!(kept >= 70, "only {kept} files");
    // Only damaged containers may be refused: calamine's and the fuzzer's.
    for name in &refused {
        assert!(
            ["OOM_alloc3.xls", "issue_714.xls"]
                .iter()
                .any(|d| name.ends_with(d))
                || name.contains("/fuzz/clusterfuzz-"),
            "{name} was refused"
        );
    }
}

#[test]
fn version_3_converts_to_version_4_and_back() {
    let v3 = fixture("v3.cfb");
    let cf = CompoundFile::open(&v3).unwrap();
    let mut b = Builder::from_file(&cf).unwrap();
    b.set_version(Version::V4);
    let v4 = b.to_bytes().unwrap();
    assert_eq!(v4.len() % 4096, 0);
    let cf4 = CompoundFile::open(&v4).unwrap();
    assert_eq!(cf4.version(), Version::V4);
    assert_eq!(content(&cf4), content(&cf));
    let mut b = Builder::from_file(&cf4).unwrap();
    b.set_version(Version::V3);
    let back = CompoundFile::open_with(&b.to_bytes().unwrap(), Default::default())
        .map(|f| content(&f))
        .unwrap();
    assert_eq!(back, content(&cf));
}

#[test]
fn builder_paths_and_errors() {
    let mut b = Builder::new(Version::V3);
    b.create_storage("/A").unwrap();
    b.create_storage_all("/A/B/C").unwrap();
    b.create_stream("/A/B/C/s", vec![1, 2, 3]).unwrap();
    assert!(matches!(
        b.create_stream("/a/b/c/S", vec![]),
        Err(CfbError::AlreadyExists { .. })
    ));
    assert!(matches!(
        b.create_stream("/Missing/s", vec![]),
        Err(CfbError::NotFound { .. })
    ));
    assert!(matches!(
        b.create_stream("/A/B/C/s/under", vec![]),
        Err(CfbError::WrongKind { .. })
    ));
    for bad in ["/A/x:y", "/A/x!y", &format!("/A/{}", "n".repeat(32))] {
        let e = b.create_stream(bad, vec![]).unwrap_err();
        assert!(matches!(e, CfbError::InvalidName { .. }), "{e}");
        assert!(!e.fix().is_empty());
    }
    assert!(matches!(
        b.create_storage("/"),
        Err(CfbError::AlreadyExists { .. })
    ));
    assert!(matches!(
        b.create_entry("/R", EntryKind::Root, Meta::default(), vec![]),
        Err(CfbError::WrongKind { .. })
    ));
    b.put_stream("/A/B/C/s", vec![9; 5000]).unwrap();
    b.put_stream("/new", vec![7]).unwrap();
    assert!(matches!(
        b.put_stream("/A", vec![]),
        Err(CfbError::WrongKind { .. })
    ));
    let meta = Meta {
        clsid: [7; 16],
        state_bits: 3,
        created: 11,
        modified: 12,
    };
    b.set_meta("/A/B", meta).unwrap();
    b.set_meta("/", meta).unwrap();
    assert_eq!(b.meta("/a/b"), Some(meta));
    assert_eq!(b.stream("/A/B/C/s").map(<[u8]>::len), Some(5000));

    let bytes = b.to_bytes().unwrap();
    let cf = CompoundFile::open(&bytes).unwrap();
    assert_eq!(cf.read_stream("/A/B/C/s").unwrap(), vec![9; 5000]);
    assert_eq!(cf.read_stream("/new").unwrap(), [7]);
    assert_eq!(cf.entry("/A/B").unwrap().meta(), meta);
    assert_eq!(cf.root().meta(), meta);
    assert!(matches!(
        cf.read_stream("/A"),
        Err(CfbError::WrongKind { .. })
    ));
    assert!(matches!(
        cf.read_stream("/nope"),
        Err(CfbError::NotFound { .. })
    ));
}

#[test]
fn an_empty_file_round_trips() {
    for v in [Version::V3, Version::V4] {
        let bytes = Builder::new(v).to_bytes().unwrap();
        assert_eq!(bytes.len(), 3 * v.sector_len());
        let cf = CompoundFile::open(&bytes).unwrap();
        assert_eq!(cf.walk("/").unwrap().len(), 1);
    }
}

#[test]
fn many_entries_and_a_difat() {
    // Over 109 FAT sectors needs DIFAT sectors: 109 * 128 sectors of 512
    // bytes is 7 MB.
    let mut b = Builder::new(Version::V3);
    let big = data(9, 7_400_000);
    b.create_stream("/Big", big.clone()).unwrap();
    for i in 0..600 {
        b.create_stream(&format!("/s{i}"), data(i, (i as usize * 13) % 5000))
            .unwrap();
    }
    let bytes = b.to_bytes().unwrap();
    assert_ne!(common::le32(&bytes, 72), 0, "no DIFAT sectors");
    let cf = CompoundFile::open(&bytes).unwrap();
    assert_eq!(cf.read_stream("/Big").unwrap(), big);
    for i in [0u32, 1, 299, 599] {
        assert_eq!(
            cf.read_stream(&format!("/S{i}")).unwrap(),
            data(i, (i as usize * 13) % 5000)
        );
    }
    assert_eq!(cf.children(cf.root()).count(), 601);
}

#[test]
fn values_the_format_says_are_zero_are_kept() {
    // A class id and times on a stream, an entry of unknown type, and an
    // entry no storage links to: MS-CFB has no use for them, but a save
    // keeps them.
    let mut b = Builder::new(Version::V3);
    b.create_stream("/odd", data(4, 50)).unwrap();
    b.create_stream("/old", data(5, 70)).unwrap();
    b.create_stream("/lost", data(6, 90)).unwrap();
    let mut bytes = b.to_bytes().unwrap();
    let odd = entry_offset(&bytes, entry_id(&bytes, "odd"));
    bytes[odd + 80..odd + 96].copy_from_slice(&[0xAB; 16]);
    set64(&mut bytes, odd + 100, 0x01D0_0000_0000_0001);
    set64(&mut bytes, odd + 108, 0x01D0_0000_0000_0002);
    let old = entry_offset(&bytes, entry_id(&bytes, "old"));
    bytes[old + 66] = 3; // type 3: ILockBytes in early OLE
    // Unlink "lost": the root's tree is lost -> (odd, old) by name order,
    // so make the root's child the middle entry and drop "lost" from it.
    let (lost, odd_id, old_id) = (
        entry_id(&bytes, "lost"),
        entry_id(&bytes, "odd") as u32,
        entry_id(&bytes, "old") as u32,
    );
    let root = entry_offset(&bytes, 0);
    set32(&mut bytes, root + 76, odd_id);
    let odd = entry_offset(&bytes, odd_id as usize);
    set32(&mut bytes, odd + 68, u32::MAX);
    set32(&mut bytes, odd + 72, old_id);
    let old = entry_offset(&bytes, old_id as usize);
    set32(&mut bytes, old + 68, u32::MAX);
    set32(&mut bytes, old + 72, u32::MAX);

    let cf = CompoundFile::open(&bytes).unwrap();
    assert_eq!(cf.entry("/odd").unwrap().clsid(), [0xAB; 16]);
    assert_eq!(cf.entry("/old").unwrap().kind(), EntryKind::Unknown(3));
    assert!(!cf.exists("/lost"));
    let detached: Vec<_> = cf.detached().map(|e| e.name().to_string()).collect();
    assert_eq!(detached, ["lost"]);
    assert_eq!(cf.detached().next().unwrap().id() as usize, lost);

    let written = Builder::from_file(&cf).unwrap().to_bytes().unwrap();
    let again = CompoundFile::open(&written).unwrap();
    let e = again.entry("/odd").unwrap();
    assert_eq!(e.clsid(), [0xAB; 16]);
    assert_eq!(e.created(), 0x01D0_0000_0000_0001);
    assert_eq!(e.modified(), 0x01D0_0000_0000_0002);
    assert_eq!(again.read(e).unwrap(), data(4, 50));
    let e = again.entry("/old").unwrap();
    assert_eq!(e.kind(), EntryKind::Unknown(3));
    assert_eq!(again.read(e).unwrap(), data(5, 70));
    let d = again.detached().next().unwrap();
    assert_eq!(d.name(), "lost");
    assert_eq!(again.read(d).unwrap(), data(6, 90));
}

#[test]
fn errors_have_three_parts() {
    let e = CompoundFile::open(b"not a compound file").unwrap_err();
    for text in [e.what(), e.why(), e.fix()] {
        assert!(!text.is_empty());
        assert!(!text.to_lowercase().contains("excel to repair"));
    }
    assert!(e.to_string().starts_with(&e.what()));
}
