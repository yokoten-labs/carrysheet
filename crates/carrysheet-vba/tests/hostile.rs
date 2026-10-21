//! Damaged and hostile input: clear errors, no panics, bounded memory.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use carrysheet_vba::{Limits, VbaError, VbaPart, read_vba_project_bin, read_xls_vba};
use common::{ModuleSpec, Rng, Streams};

fn simple_modules() -> Vec<ModuleSpec> {
    vec![
        ModuleSpec::document(
            "ThisWorkbook",
            "Attribute VB_Name = \"ThisWorkbook\"\r\nPrivate Sub Workbook_Open()\r\nShell \"calc\"\r\nEnd Sub\r\n",
        ),
        ModuleSpec::standard(
            "Module1",
            "Attribute VB_Name = \"Module1\"\r\nSub A()\r\nEnd Sub\r\n",
        ),
    ]
}

fn assert_explained(e: &VbaError) {
    assert!(!e.what().is_empty() && !e.why().is_empty() && !e.fix().is_empty());
    let text = e.to_string();
    assert!(text.contains(&e.what()) && text.contains(&e.fix()));
}

#[test]
fn not_a_compound_file() {
    for (bytes, hint) in [
        (&b""[..], "empty"),
        (&b"PK\x03\x04rest of a zip"[..], "xl/vbaProject.bin"),
        (&b"Attribute VB_Name"[..], "OLE2 signature"),
    ] {
        match read_vba_project_bin(bytes) {
            Err(e @ VbaError::NotCompoundFile { .. }) => {
                assert!(e.why().contains(hint), "{e}");
                assert_explained(&e);
            }
            other => panic!("expected NotCompoundFile, got {other:?}"),
        }
    }
}

#[test]
fn xls_without_macros_is_none() {
    let mut cf = carrysheet_vba::cfb::Builder::new(carrysheet_vba::cfb::Version::V3);
    cf.create_stream("/Workbook", vec![9, 8, 0, 0]).unwrap();
    let bytes = cf.to_bytes().unwrap();
    assert_eq!(read_xls_vba(&bytes).unwrap(), None);
}

#[test]
fn truncated_container_is_a_container_error() {
    let bin = common::data("SimpleMacro-vbaProject.bin");
    for len in [8, 512, 600, 4096, bin.len() / 2] {
        match read_vba_project_bin(&bin[..len]) {
            Err(e @ (VbaError::Container { .. } | VbaError::MissingStream { .. })) => {
                assert_explained(&e)
            }
            other => panic!("len {len}: expected an error, got {other:?}"),
        }
    }
}

#[test]
fn missing_dir_stream() {
    let mut s = Streams::new(1252, &simple_modules());
    s.remove("VBA/dir");
    match read_vba_project_bin(&s.to_bin()) {
        Err(e @ VbaError::MissingStream { .. }) => {
            assert!(e.what().contains("VBA/dir"));
            assert_explained(&e);
        }
        other => panic!("expected MissingStream, got {other:?}"),
    }
}

#[test]
fn missing_project_stream_still_lists_modules() {
    let mut s = Streams::new(1252, &simple_modules());
    s.remove("PROJECT");
    let p = read_vba_project_bin(&s.to_bin()).unwrap();
    assert!(!p.has_project_stream);
    assert_eq!(p.modules.len(), 2);
    // Without PROJECT nothing can be called hidden.
    assert!(p.modules.iter().all(|m| !m.hidden));
}

#[test]
fn missing_module_stream_spares_the_others() {
    let mut s = Streams::new(1252, &simple_modules());
    s.remove("VBA/Module1");
    let p = read_vba_project_bin(&s.to_bin()).unwrap();
    assert!(p.modules[0].has_source);
    match &p.modules[1].error {
        Some(e @ VbaError::MissingStream { stream }) => {
            assert_eq!(stream, "VBA/Module1");
            assert_explained(e);
        }
        other => panic!("expected MissingStream, got {other:?}"),
    }
}

#[test]
fn module_offset_past_the_end() {
    let mut modules = simple_modules();
    modules[1].pcode = 100;
    let mut s = Streams::new(1252, &modules);
    // The dir says source starts at 100; cut the stream to 50 bytes.
    s.set("VBA/Module1", vec![0; 50]);
    let p = read_vba_project_bin(&s.to_bin()).unwrap();
    assert_eq!(
        p.modules[1].error,
        Some(VbaError::BadModuleOffset {
            module: "Module1".into(),
            offset: 100,
            stream_len: 50
        })
    );
    assert!(p.modules[1].source.is_none());
}

#[test]
fn huge_record_size_is_an_error_not_an_allocation() {
    let mut dir = common::dir_stream(1252, &simple_modules());
    // The NAME record (id 0x0004) follows SYSKIND, LCID, LCIDINVOKE and
    // CODEPAGE: 10 + 10 + 10 + 8 bytes in. Claim 4 GB.
    assert_eq!(&dir[38..40], &[0x04, 0x00]);
    dir[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut s = Streams::new(1252, &simple_modules());
    s.set("VBA/dir", common::compress_literal(&dir));
    match read_vba_project_bin(&s.to_bin()) {
        Err(e @ VbaError::BadDir { offset: 40, .. }) => assert_explained(&e),
        other => panic!("expected BadDir, got {other:?}"),
    }
}

#[test]
fn source_bomb_hits_the_limit() {
    let mut s = Streams::new(1252, &simple_modules());
    let mut stream = vec![0; 700];
    stream.extend(common::bomb(1000)); // 6 KB that expands to 4 MB
    s.set("VBA/Module1", stream);
    let limits = Limits {
        max_decompressed_bytes: 1 << 20,
        ..Limits::default()
    };
    match VbaPart::from_bin(s.to_bin()).read(&limits) {
        Err(e @ VbaError::LimitExceeded { .. }) => assert_explained(&e),
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
}

/// fuzzlite finding (2026-10-20, target `vba`, kind amplification): a
/// bomb just under the 16 MiB per-stream limit behind each of 16 modules.
/// The 479 KB file decompresses to 256 MiB of source, inside the default
/// limits, and reading took 12.7 s and about 300 MiB in a release build.
/// Decompressing is fast (26 ms per 16 MiB); the time goes into what
/// follows for each 16 MiB source (about 0.8 s each). Run with
/// `cargo test --release -p carrysheet-vba --test hostile -- --ignored`.
#[test]
#[ignore = "fuzzlite finding: 16 module bombs take 12.7 s to read; not fixed yet"]
fn bomb_behind_every_module_reads_fast() {
    let modules: Vec<ModuleSpec> = (0..16)
        .map(|i| ModuleSpec::standard(&format!("M{i}"), "x"))
        .collect();
    let mut s = Streams::new(1252, &modules);
    for i in 0..16 {
        let mut stream = vec![0; 700];
        stream.extend(common::bomb(4095)); // 24 KB that expands to 16 MiB
        s.set(&format!("VBA/M{i}"), stream);
    }
    let bin = s.to_bin();
    assert!(bin.len() < 512 << 10);
    let start = std::time::Instant::now();
    let _ = read_vba_project_bin(&bin);
    let took = start.elapsed();
    assert!(
        took < std::time::Duration::from_secs(2),
        "a {}-byte file took {took:?}",
        bin.len()
    );
}

#[test]
fn counts_are_limited() {
    let modules: Vec<ModuleSpec> = (0..5)
        .map(|i| ModuleSpec::standard(&format!("M{i}"), "Sub A()\r\nEnd Sub\r\n"))
        .collect();
    let bin = Streams::new(1252, &modules).to_bin();
    let part = VbaPart::from_bin(bin);
    let tight = |l: Limits| part.read(&l);
    assert!(matches!(
        tight(Limits {
            max_modules: 4,
            ..Limits::default()
        }),
        Err(VbaError::LimitExceeded {
            limit: "modules",
            ..
        })
    ));
    assert!(matches!(
        tight(Limits {
            max_entries: 3,
            ..Limits::default()
        }),
        Err(VbaError::LimitExceeded { .. })
    ));
    assert!(matches!(
        tight(Limits {
            max_stream_bytes: 100,
            ..Limits::default()
        }),
        Err(VbaError::LimitExceeded { .. })
    ));
    assert!(matches!(
        tight(Limits {
            max_input_bytes: 100,
            ..Limits::default()
        }),
        Err(VbaError::LimitExceeded { .. })
    ));
    let p = tight(Limits {
        max_flags: 0,
        ..Limits::default()
    })
    .unwrap();
    assert!(p.flags.is_empty());
}

/// Runs a read and fails the test if anything panics outside the
/// compound file guard.
fn must_not_panic(what: &str, i: usize, f: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        panic!("{what}: mutation {i} panicked");
    }
}

#[test]
fn mutation_fixed_seed() {
    let limits = Limits {
        max_decompressed_bytes: 1 << 20,
        ..Limits::default()
    };
    let mut rng = Rng::new(0xCA77_5EED);
    let modules = simple_modules();
    let base = Streams::new(1252, &modules);

    // 1. The decompressed dir stream: the record parser.
    let dir = common::dir_stream(1252, &modules);
    let (mut ok, mut bad_dir) = (0, 0);
    for i in 0..600 {
        let mut d = dir.clone();
        rng.mutate(&mut d);
        let mut s = Streams(base.0.clone());
        s.set("VBA/dir", common::compress_literal(&d));
        let bin = s.to_bin();
        must_not_panic("dir", i, || match VbaPart::from_bin(bin).read(&limits) {
            Ok(_) => ok += 1,
            Err(VbaError::BadDir { .. }) => bad_dir += 1,
            Err(_) => {}
        });
    }
    // The mutations reach the parser: some still read, some are rejected.
    assert!(ok > 50 && bad_dir > 50, "ok {ok}, bad dir {bad_dir}");

    // 2. The compressed streams: dir, a module, PROJECT.
    for (path, n) in [
        ("VBA/dir", 300),
        ("VBA/ThisWorkbook", 300),
        ("PROJECT", 300),
    ] {
        let original = base.0.iter().find(|(p, _)| p == path).unwrap().1.clone();
        for i in 0..n {
            let mut bytes = original.clone();
            rng.mutate(&mut bytes);
            let mut s = Streams(base.0.clone());
            s.set(path, bytes);
            let bin = s.to_bin();
            must_not_panic(path, i, || {
                let _ = VbaPart::from_bin(bin).read(&limits);
            });
        }
    }

    // 3. Whole files: the container itself.
    let bin = common::data("SimpleMacro-vbaProject.bin");
    let xls = common::data("SimpleMacro.xls");
    for i in 0..300 {
        let mut b = bin.clone();
        rng.mutate(&mut b);
        must_not_panic("vbaProject.bin", i, || {
            let _ = VbaPart::from_bin(b).read(&limits);
        });
        let mut x = xls.clone();
        rng.mutate(&mut x);
        must_not_panic(".xls", i, || {
            if let Ok(Some(part)) = VbaPart::from_xls(&x, &limits) {
                let _ = part.read(&limits);
            }
        });
    }
}

#[test]
fn shared_failing_stream_is_decompressed_once() {
    // 4096 modules point at one stream whose source decompresses to just
    // under 16 MiB and then breaks. Each must not cost a new decompression.
    let modules: Vec<ModuleSpec> = (0..4096)
        .map(|i| {
            let mut m = ModuleSpec::standard(&format!("M{i}"), "");
            m.stream = "Shared".into();
            m.pcode = 0;
            m
        })
        .collect();
    let mut s = Streams::new(1252, &modules);
    let mut bomb = common::bomb(4095); // 4095 * 4096 = 16 MiB - 4 KiB
    bomb.extend_from_slice(&[0x00, 0x00]); // a chunk with a bad signature
    s.set("VBA/Shared", bomb);
    let bin = s.to_bin();

    let started = std::time::Instant::now();
    let p = read_vba_project_bin(&bin).unwrap();
    let took = started.elapsed();
    eprintln!("shared failing stream: {took:?}");
    assert_eq!(p.modules.len(), 4096);
    assert!(
        p.modules
            .iter()
            .all(|m| matches!(m.error, Some(VbaError::BadCompression { .. })))
    );
    assert!(took.as_millis() < 1000, "took {took:?}");
}

#[test]
fn shared_large_source_hits_the_text_budget() {
    // One 1 MiB source shared by 400 modules would hand out 400 MiB of text.
    let modules: Vec<ModuleSpec> = (0..400)
        .map(|i| {
            let mut m = ModuleSpec::standard(&format!("M{i}"), "");
            m.stream = "Shared".into();
            m.pcode = 0;
            m
        })
        .collect();
    let mut s = Streams::new(1252, &modules);
    s.set("VBA/Shared", common::bomb(256));
    match read_vba_project_bin(&s.to_bin()) {
        Err(VbaError::LimitExceeded { limit, .. }) => {
            assert_eq!(limit, "source text in the project")
        }
        other => panic!(
            "expected the text limit, got {:?}",
            other.map(|p| p.modules.len())
        ),
    }
}

#[test]
fn probe_overlapping_offsets_into_one_stream() {
    // `01 00 B0` repeated is a valid container at every multiple of 3 (each
    // chunk produces nothing). 256 modules starting at 0, 3, 6... would each
    // walk the rest of a 16 MiB stream.
    let modules: Vec<ModuleSpec> = (0..256)
        .map(|i| {
            let mut m = ModuleSpec::standard(&format!("M{i}"), "");
            m.stream = "Shared".into();
            m.pcode = 3 * i;
            m
        })
        .collect();
    let mut s = Streams::new(1252, &modules);
    let len = (16 << 20) / 3 * 3;
    s.set(
        "VBA/Shared",
        [0x01u8, 0x00, 0xB0]
            .iter()
            .copied()
            .cycle()
            .take(len)
            .collect(),
    );
    let bin = s.to_bin();
    let started = std::time::Instant::now();
    let result = read_vba_project_bin(&bin);
    let took = started.elapsed();
    eprintln!("overlapping offsets: {took:?}");
    match result {
        Err(e @ VbaError::LimitExceeded { .. }) => {
            assert!(e.what().contains("compressed source bytes"), "{e}");
            assert!(e.fix().contains("hostile"));
        }
        other => panic!(
            "expected the limit, got {:?}",
            other.map(|p| p.modules.len())
        ),
    }
    assert!(took.as_millis() < 500, "took {took:?}");
}

#[test]
fn probe_many_sources_full_of_hits() {
    // 400 distinct sources of 10,240 `Kill a` lines each: once the flag list
    // is full, later sources are not scanned and keep no hits.
    let n = 400;
    let modules: Vec<ModuleSpec> = (0..n)
        .map(|i| {
            let mut m = ModuleSpec::standard(&format!("M{i}"), "");
            m.pcode = 0;
            m
        })
        .collect();
    let mut s = Streams::new(1252, &modules);
    let mut packed = vec![0x01u8];
    for _ in 0..20 {
        // 8 literals, then copy 4088 bytes from 8 back: 512 lines per chunk.
        packed.extend_from_slice(&(0xB000u16 | 11).to_le_bytes());
        packed.push(0x00);
        packed.extend_from_slice(b"Kill a\r\n");
        packed.push(0x01);
        packed.extend_from_slice(&((7u16 << 12) | 4085).to_le_bytes());
    }
    for i in 0..n {
        s.set(&format!("VBA/M{i}"), packed.clone());
    }
    let bin = s.to_bin();
    let started = std::time::Instant::now();
    let p = read_vba_project_bin(&bin).unwrap();
    let took = started.elapsed();
    eprintln!("400 sources full of hits: {took:?}");
    assert_eq!(p.modules[0].line_count, 10_240);
    // The list is full, and one more flag says so.
    let max = Limits::default().max_flags;
    assert_eq!(p.flags.len(), max + 1);
    assert_eq!(p.flags[max].rule, "Flag list cut short");
    assert!(p.flags.iter().all(|f| f.module_index == 0));
    assert!(took.as_secs() < 10, "took {took:?}");
}

// ---- Compound file review round 1 ----

use carrysheet_vba::cfb::{Builder, EntryKind, Meta, NodeId, Version};

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// A VBA storage nested `depth` storages deep, with `streams` streams at
/// the bottom; every name is 31 code units.
fn deep_xls(depth: usize, streams: usize) -> Vec<u8> {
    let long = |i: usize| {
        let mut u = vec!['名' as u16; 23];
        u.extend(utf16(&format!("{i:08}")));
        u
    };
    let mut b = Builder::new(Version::V4);
    let mut at = b
        .create_child(
            NodeId::ROOT,
            &utf16("_VBA_PROJECT_CUR"),
            EntryKind::Storage,
            Meta::default(),
            vec![],
        )
        .unwrap();
    for d in 0..depth {
        at = b
            .create_child(at, &long(d), EntryKind::Storage, Meta::default(), vec![])
            .unwrap();
    }
    for i in 0..streams {
        b.create_child(at, &long(i), EntryKind::Stream, Meta::default(), vec![])
            .unwrap();
    }
    b.to_bytes().unwrap()
}

#[test]
fn entries_are_counted_before_paths_are_built() {
    let xls = deep_xls(61, 3_000);
    let r = VbaPart::from_xls(
        &xls,
        &Limits {
            max_entries: 1_000,
            ..Limits::default()
        },
    );
    assert!(
        matches!(
            r,
            Err(VbaError::LimitExceeded {
                limit: "entries in the VBA storage",
                ..
            })
        ),
        "{r:?}"
    );
}

#[test]
fn long_paths_count_against_the_storage_budget() {
    // 61 storages of 31-character names: each stream path is about 5 KB
    // of UTF-8, with no stream bytes at all.
    let xls = deep_xls(61, 1_000);
    let r = VbaPart::from_xls(
        &xls,
        &Limits {
            max_storage_bytes: 1 << 20,
            ..Limits::default()
        },
    );
    assert!(
        matches!(
            r,
            Err(VbaError::LimitExceeded {
                limit: "bytes in the VBA storage",
                ..
            })
        ),
        "{r:?}"
    );
    assert!(VbaPart::from_xls(&xls, &Limits::default()).is_ok());
}

#[test]
fn an_unreadable_entry_of_unknown_type_is_an_error_not_empty_bytes() {
    let mut b = Builder::new(Version::V3);
    b.create_storage("/_VBA_PROJECT_CUR").unwrap();
    b.create_entry(
        "/_VBA_PROJECT_CUR/Odd",
        EntryKind::Unknown(3),
        Meta::default(),
        vec![9; 5000],
    )
    .unwrap();
    let mut xls = b.to_bytes().unwrap();
    // Make it claim more bytes than the file has.
    let name: Vec<u8> = utf16("Odd\0")
        .iter()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let at = xls.windows(name.len()).position(|w| w == name).unwrap();
    let size = xls.len() as u64 * 2;
    xls[at + 120..at + 128].copy_from_slice(&size.to_le_bytes());
    let r = VbaPart::from_xls(&xls, &Limits::default());
    assert!(matches!(r, Err(VbaError::Container { .. })), "{r:?}");
}

#[test]
fn names_are_written_back_exactly() {
    // A slash and half a surrogate pair: nothing a path can carry.
    let names = [utf16("a/b"), vec![0x41, 0xD800, 0x42]];
    let mut b = Builder::new(Version::V3);
    let s = b
        .create_child(
            NodeId::ROOT,
            &utf16("_VBA_PROJECT_CUR"),
            EntryKind::Storage,
            Meta::default(),
            vec![],
        )
        .unwrap();
    let inner = b
        .create_child(s, &names[0], EntryKind::Storage, Meta::default(), vec![])
        .unwrap();
    b.create_child(
        inner,
        &names[1],
        EntryKind::Stream,
        Meta::default(),
        vec![5; 40],
    )
    .unwrap();
    let xls = b.to_bytes().unwrap();
    let part = VbaPart::from_xls(&xls, &Limits::default())
        .unwrap()
        .unwrap();
    let kept = part.storage().unwrap();
    assert_eq!(kept.entries()[2].parent(), Some(1));
    let mut out = Builder::new(Version::V3);
    kept.write_into(&mut out, "/_VBA_PROJECT_CUR").unwrap();
    assert_eq!(out.to_bytes().unwrap(), xls);
}

// ---- Compound file review round 2 ----

/// Drops the terminating null of the directory entry named `name` from
/// its name length: the stored name keeps the same units, unterminated.
fn unterminate(bytes: &mut [u8], name: &str) {
    let ss = 1usize << u16::from_le_bytes([bytes[30], bytes[31]]);
    let dir = (u32::from_le_bytes(bytes[48..52].try_into().unwrap()) as usize + 1) * ss;
    let mut want: Vec<u8> = utf16(name).iter().flat_map(|u| u.to_le_bytes()).collect();
    want.extend([0, 0]);
    let at = (dir..bytes.len() - 128)
        .step_by(128)
        .find(|&at| bytes[at..at + want.len()] == want[..])
        .unwrap();
    let len = (want.len() - 2) as u16;
    bytes[at + 64..at + 66].copy_from_slice(&len.to_le_bytes());
}

#[test]
fn a_name_without_its_null_reads_but_is_not_written_back() {
    let mut bin = Streams::new(1252, &simple_modules()).to_bin();
    unterminate(&mut bin, "PROJECT");

    // Reading the macros works.
    let p = read_vba_project_bin(&bin).unwrap();
    assert_eq!(p.modules.len(), 2);

    // The same through an .xls and a kept storage.
    let file = carrysheet_vba::cfb::CompoundFile::open(&bin).unwrap();
    assert!(!file.entry("/PROJECT").unwrap().name_terminated());
    let part = VbaPart::from_storage(&file, "/", &Limits::default()).unwrap();
    assert_eq!(part.read(&Limits::default()).unwrap().modules.len(), 2);

    // Writing it back refuses, before anything is written.
    let mut out = Builder::new(Version::V3);
    out.create_stream("/Workbook", vec![9, 8, 0, 0]).unwrap();
    let before = out.clone();
    match part
        .storage()
        .unwrap()
        .write_into(&mut out, "/_VBA_PROJECT_CUR")
    {
        Err(e @ VbaError::WriteFailed { .. }) => {
            assert!(e.to_string().contains("no terminating null"), "{e}");
            assert_explained(&e);
        }
        other => panic!("expected a write error, got {other:?}"),
    }
    assert_eq!(out, before);
}

#[test]
fn an_xls_with_an_unterminated_name_lists_its_macros() {
    // A VBA storage in an .xls, then one of its names unterminated.
    let bin = Streams::new(1252, &simple_modules()).to_bin();
    let cf = carrysheet_vba::cfb::CompoundFile::open(&bin).unwrap();
    let mut b = Builder::new(Version::V3);
    b.create_stream("/Workbook", vec![9, 8, 0, 0]).unwrap();
    VbaPart::from_storage(&cf, "/", &Limits::default())
        .unwrap()
        .storage()
        .unwrap()
        .write_into(&mut b, "/_VBA_PROJECT_CUR")
        .unwrap();
    let mut xls = b.to_bytes().unwrap();
    unterminate(&mut xls, "PROJECT");
    let p = read_xls_vba(&xls).unwrap().expect("macros");
    assert_eq!(p.modules.len(), 2);
}
