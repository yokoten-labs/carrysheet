//! Well-formed archives read back as written.

mod common;

use std::io::Cursor;

use carrysheet_zip::{Archive, DuplicateNames, Error, Limits, Method as M};
use common::*;

fn open(bytes: Vec<u8>) -> Result<Archive<Cursor<Vec<u8>>>, Error> {
    Archive::open(Cursor::new(bytes), Limits::default())
}

#[test]
fn stored_and_deflated_entries_read_back() {
    let text = b"hello, hello, hello, hello".repeat(1000);
    let bytes = zip(&[
        ("a.txt", b"plain", Method::Stored),
        ("dir/", b"", Method::Stored),
        ("dir/b.xml", &text, Method::Deflated),
        ("empty", b"", Method::Deflated),
    ]);
    let mut a = open(bytes).unwrap();
    let names: Vec<_> = a.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["a.txt", "dir/", "dir/b.xml", "empty"]);
    assert!(a.entries()[1].is_dir());
    assert_eq!(a.entries()[2].method, M::Deflated);
    assert_eq!(a.read(0, 100).unwrap(), b"plain");
    let b = a.find("dir/b.xml").unwrap();
    assert_eq!(a.read(b, 1 << 20).unwrap(), text);
    assert_eq!(a.read(3, 10).unwrap(), b"");
    assert_eq!(a.read_prefix(b, 5).unwrap(), b"hello");
    assert_eq!(a.read_prefix(0, 100).unwrap(), b"plain");
    assert!(a.bytes_read() > 0);
}

#[test]
fn names_are_found_exactly_or_in_any_ascii_case() {
    let mut b = Builder::new();
    b.add("xl/vbaProject.bin", b"1", Method::Stored)
        .add("XL/VBAPROJECT.BIN", b"2", Method::Stored)
        .add("[Content_Types].xml", b"3", Method::Stored);
    let a = open(b.finish()).unwrap();
    assert_eq!(a.find("xl/vbaProject.bin"), Some(0));
    assert_eq!(a.find("XL/VBAPROJECT.BIN"), Some(1));
    assert_eq!(a.find("xl/vbaproject.bin"), None);
    assert_eq!(a.find_ignore_case("xl/vbaproject.bin"), Some(0));
    assert_eq!(a.find_ignore_case("XL/VBAPROJECT.BIN"), Some(1));
    assert_eq!(a.find_ignore_case("[content_types].XML"), Some(2));
    assert_eq!(a.case_twins(), [(0, 1)]);
    assert!(a.duplicates().is_empty());
}

#[test]
fn empty_archives_comments_and_data_after_are_fine() {
    let empty = end_record(0, 0, 0, 0);
    assert_eq!(open(empty.clone()).unwrap().entries().len(), 0);

    let mut commented = zip(&[("a", b"x", Method::Stored)]);
    let n = commented.len();
    commented[n - 2..].copy_from_slice(&5u16.to_le_bytes());
    commented.extend_from_slice(b"hello");
    assert_eq!(open(commented).unwrap().read(0, 9).unwrap(), b"x");

    // Bytes after the archive, as some tools append, within reach.
    let mut after = zip(&[("a", b"x", Method::Stored)]);
    after.extend(std::iter::repeat_n(7u8, 60_000));
    assert_eq!(open(after).unwrap().read(0, 9).unwrap(), b"x");
}

#[test]
fn zip64_archives_and_data_descriptors_read() {
    let mut b = Builder::new();
    b.add("a.txt", b"zip64", Method::Deflated);
    // An entry whose sizes live in a zip64 extra field.
    let mut big = Raw::new("b.txt", b"sixty-four", Method::Stored);
    big.compressed_size = u32::MAX;
    big.size = u32::MAX;
    big.central_extra = zip64_extra(&[10, 10]);
    big.local_compressed_size = u32::MAX;
    big.local_size = u32::MAX;
    big.local_extra = zip64_extra(&[10, 10]);
    b.raw(big);
    // An entry written with a data descriptor: zeros in the local header.
    let mut streamed = Raw::new("c.txt", b"streamed", Method::Deflated).flags(1 << 3);
    streamed.local_compressed_size = 0;
    streamed.local_size = 0;
    let crc = streamed.crc;
    let mut descriptor = b"PK\x07\x08".to_vec();
    descriptor.extend_from_slice(&crc.to_le_bytes());
    descriptor.extend_from_slice(&(streamed.data.len() as u32).to_le_bytes());
    descriptor.extend_from_slice(&8u32.to_le_bytes());
    let offset = b.out.len() as u32;
    b.out.extend_from_slice(&streamed.local_header());
    b.out.extend_from_slice(&streamed.data);
    b.out.extend_from_slice(&descriptor);
    b.central_only(&streamed, offset);
    let mut a = open(b.finish_zip64()).unwrap();
    assert_eq!(a.read(0, 99).unwrap(), b"zip64");
    assert_eq!(a.entries()[1].size, 10);
    assert_eq!(a.read(1, 99).unwrap(), b"sixty-four");
    assert_eq!(a.read(2, 99).unwrap(), b"streamed");
}

#[test]
fn names_decode_as_utf8_or_code_page_437() {
    let mut b = Builder::new();
    let mut flagged = Raw::new("Módulo.bas", b"", Method::Stored).flags(1 << 11);
    flagged.name = "Módulo.bas".as_bytes().to_vec();
    b.raw(flagged);
    let mut dos = Raw::new("x", b"", Method::Stored);
    dos.name = b"caf\x82.txt".to_vec();
    dos.local_name = dos.name.clone();
    b.raw(dos);
    let a = open(b.finish()).unwrap();
    assert_eq!(a.entries()[0].name, "Módulo.bas");
    assert_eq!(a.entries()[1].name, "café.txt");
}

#[test]
fn duplicate_names_are_refused_or_reported() {
    let mut b = Builder::new();
    b.add("a", b"1", Method::Stored)
        .add("a", b"2", Method::Stored);
    let bytes = b.finish();
    let e = open(bytes.clone()).unwrap_err();
    assert!(matches!(e, Error::Damaged(_)), "{e}");
    let limits = Limits {
        duplicate_names: DuplicateNames::Report,
        ..Limits::default()
    };
    let mut a = Archive::open(Cursor::new(bytes), limits).unwrap();
    assert_eq!(a.duplicates(), [(0, 1)]);
    assert_eq!(a.find("a"), Some(0));
    assert_eq!(a.read(1, 9).unwrap(), b"2");
}

/// Layouts real writers produce (from the reviewer's edge probe).
#[test]
fn local_header_variants_from_real_writers_read() {
    let content = b"<?xml version=\"1.0\"?><Types/>".repeat(40);
    let check = |raw: Raw| {
        let mut b = Builder::new();
        b.raw(raw);
        assert_eq!(open(b.finish()).unwrap().read(0, 1 << 20).unwrap(), content);
    };
    // APPNOTE 4.5.3: only the compressed size is all ones, and the local
    // zip64 extra holds both sizes.
    let mut r = Raw::new("a.xml", &content, Method::Deflated);
    let csize = u64::from(r.compressed_size);
    r.local_compressed_size = u32::MAX;
    r.local_extra = zip64_extra(&[content.len() as u64, csize]);
    check(r);
    // A data descriptor, with all ones and no extra field locally.
    let mut r = Raw::new("a.xml", &content, Method::Deflated).flags(1 << 3);
    r.local_compressed_size = u32::MAX;
    r.local_size = u32::MAX;
    check(r);
    // The data-descriptor flag in the directory only, real local sizes.
    let mut r = Raw::new("a.xml", &content, Method::Deflated);
    r.flags = 1 << 3;
    check(r);
}
