//! Level 1, keep: reading macros never changes their bytes, and the kept
//! project writes back identical.

mod common;

use std::io::{Cursor, Read};

use carrysheet_vba::cfb::{Builder, CompoundFile, Version};
use carrysheet_vba::{KeptStorage, Limits, VbaPart, XLS_VBA_STORAGE};

#[test]
fn vba_project_bin_comes_back_byte_identical() {
    for name in ["SimpleMacro-vbaProject.bin", "62624.bin"] {
        let original = common::data(name);
        let part = VbaPart::from_bin(original.clone());
        // Reading must not touch the kept bytes.
        let project = part.read(&Limits::default()).unwrap();
        assert!(!project.modules.is_empty());
        let written: Vec<u8> = part.bin_bytes().unwrap().to_vec();
        assert_eq!(written, original, "{name}");
        assert!(part.storage().is_none());
    }
}

/// Every stream under `storage`, read by the independent `cfb` crate
/// (tests only), so a writer bug the reader shares cannot hide.
fn streams_of(bytes: &[u8], storage: &str) -> Vec<(String, Vec<u8>)> {
    let mut cf = ::cfb::CompoundFile::open_strict(Cursor::new(bytes)).unwrap();
    let entries: Vec<_> = cf.walk_storage(storage).unwrap().collect();
    let mut out = Vec::new();
    for e in entries {
        if e.is_stream() {
            let mut data = Vec::new();
            cf.open_stream(e.path())
                .unwrap()
                .read_to_end(&mut data)
                .unwrap();
            let path = e.path().to_string_lossy().replace('\\', "/");
            let rel = path
                .strip_prefix(storage.trim_end_matches('/'))
                .unwrap_or(&path)
                .trim_start_matches('/')
                .to_string();
            out.push((rel, data));
        }
    }
    out.sort();
    out
}

fn assert_same_storage(a: &KeptStorage, b: &KeptStorage) {
    assert_eq!(a.entries().len(), b.entries().len());
    for (x, y) in a.entries().iter().zip(b.entries()) {
        assert_eq!(x.path(), y.path());
        assert_eq!(x.data(), y.data(), "{}", x.path());
        assert_eq!(x.clsid(), y.clsid(), "{}", x.path());
        assert_eq!(x.state_bits(), y.state_bits(), "{}", x.path());
        // Times too, on storages and streams alike.
        assert_eq!(x.entry().meta(), y.entry().meta(), "{}", x.path());
    }
}

#[test]
fn xls_vba_storage_writes_back_identical() {
    for name in ["SimpleMacro.xls", "60273.xls", "59830.xls"] {
        let xls = common::data(name);
        let limits = Limits::default();
        let part = VbaPart::from_xls(&xls, &limits).unwrap().unwrap();
        let before = part.read(&limits).unwrap();
        let kept = part.storage().unwrap();
        assert!(kept.entries()[0].path().is_empty());
        assert!(kept.stream("VBA/dir").is_some());

        // Save into a new compound file, as an .xls writer would.
        let mut out = Builder::new(Version::V3);
        kept.write_into(&mut out, XLS_VBA_STORAGE).unwrap();
        let new_bytes = out.to_bytes().unwrap();

        // Same streams, byte for byte, checked without this crate's reader...
        assert_eq!(
            streams_of(&new_bytes, XLS_VBA_STORAGE),
            streams_of(&xls, XLS_VBA_STORAGE),
            "{name}"
        );
        // ...and the same metadata and project through it.
        let again = VbaPart::from_xls(&new_bytes, &limits).unwrap().unwrap();
        assert_same_storage(kept, again.storage().unwrap());
        assert_eq!(again.read(&limits).unwrap(), before, "{name}");
    }
}

#[test]
fn xls_storage_can_become_a_vba_project_bin() {
    // Saving .xls as .xlsm: the same streams under a new root.
    let xls = common::data("SimpleMacro.xls");
    let limits = Limits::default();
    let part = VbaPart::from_xls(&xls, &limits).unwrap().unwrap();
    let mut out = Builder::new(Version::V3);
    part.storage().unwrap().write_into(&mut out, "/").unwrap();
    let bin = out.to_bytes().unwrap();
    assert_eq!(streams_of(&bin, "/"), streams_of(&xls, XLS_VBA_STORAGE));
    let from_bin = VbaPart::from_bin(bin).read(&limits).unwrap();
    assert_eq!(from_bin, part.read(&limits).unwrap());
}

#[test]
fn from_storage_on_an_open_file() {
    let xls = common::data("SimpleMacro.xls");
    let cf = CompoundFile::open(&xls).unwrap();
    let part = VbaPart::from_storage(&cf, XLS_VBA_STORAGE, &Limits::default()).unwrap();
    assert_eq!(part.read(&Limits::default()).unwrap().modules.len(), 5);
}
