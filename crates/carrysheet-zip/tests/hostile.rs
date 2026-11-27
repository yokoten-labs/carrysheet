//! Archives built to make a reader search, allocate or inflate without
//! end, or to show it a different file than other readers see. Each is
//! built in code; the reviewer's probes (end records far from the end,
//! entries sharing one stream of empty blocks, a zip64 decoy in a
//! sparse 8 GB file) are among them.

mod common;

use std::io::Cursor;
use std::time::{Duration, Instant};

use carrysheet_zip::{Archive, Error, Limits};
use common::*;

fn open(bytes: Vec<u8>) -> Result<Archive<Cursor<Vec<u8>>>, Error> {
    Archive::open(Cursor::new(bytes), Limits::default())
}

fn damaged<T: std::fmt::Debug>(r: Result<T, Error>, says: &str) {
    match r {
        Err(Error::Damaged(what)) => assert!(what.contains(says), "{what}"),
        other => panic!("expected damaged ({says}), got {other:?}"),
    }
}

fn slow<T: std::fmt::Debug>(r: Result<T, Error>, says: &str) {
    match r {
        Err(Error::Slow(what)) => assert!(what.contains(says), "{what}"),
        other => panic!("expected slow ({says}), got {other:?}"),
    }
}

fn over_limit<T: std::fmt::Debug>(r: Result<T, Error>, says: &str) {
    match r {
        Err(Error::LimitExceeded { what, .. }) => assert!(what.contains(says), "{what}"),
        other => panic!("expected a limit ({says}), got {other:?}"),
    }
}

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = f();
    let took = started.elapsed();
    eprintln!("{what}: {took:?}");
    assert!(took < Duration::from_secs(1), "{what}: {took:?}");
    out
}

/// Reviewer probe B1: `records` end records far from the end of the
/// file, each pointing at a directory of `claimed - 1` real headers
/// while claiming `claimed`. A reader that retries each record parses
/// the whole directory every time.
fn end_records_far_from_the_end(records: usize, claimed: u16, after: usize) -> Vec<u8> {
    let raw = Raw::new("a", b"", Method::Stored);
    let mut b = raw.local_header();
    let directory_at = b.len() as u32;
    let header = raw.central_header(0);
    let mut directory = Vec::new();
    for _ in 1..claimed {
        directory.extend_from_slice(&header);
    }
    b.extend_from_slice(&directory);
    for _ in 0..records {
        b.extend_from_slice(&end_record(
            claimed,
            directory.len() as u32,
            directory_at,
            0,
        ));
    }
    b.resize(b.len() + after, 0);
    b
}

#[test]
fn end_records_are_looked_for_once_and_only_near_the_end() {
    // 1,000 records, then 70,000 bytes: none is in reach.
    let probe = end_records_far_from_the_end(1_000, 65_534, 70_000);
    let r = timed("1,000 far end records over 3 MB", || open(probe));
    damaged(r, "no end-of-central-directory record");

    // The same records at the end: the last one is used, and only it.
    let probe = end_records_far_from_the_end(1_000, 65_534, 0);
    let r = timed("1,000 end records claiming 65,534", || open(probe));
    over_limit(r, "zip entries");
    let probe = end_records_far_from_the_end(1_000, 2_000, 0);
    let r = timed("1,000 end records, one header short", || open(probe));
    damaged(r, "does not end where the end record begins");

    // An end record whose comment would run past the file is not valid;
    // the one before it is used.
    let mut good = zip(&[("a", b"x", Method::Stored)]);
    good.extend_from_slice(&end_record(0, 0, 0, 500));
    assert_eq!(open(good).unwrap().entries().len(), 1);
}

/// Reviewer probe B2: one stream of empty deflate blocks, with many
/// directory entries pointing at it.
#[test]
fn entries_sharing_data_are_refused_before_any_inflating() {
    let stream = empty_blocks((4 << 20) / 5);
    let mut raw = Raw::new("x.bin", b"", Method::Deflated);
    raw.data = stream.clone();
    raw.compressed_size = stream.len() as u32;
    raw.local_compressed_size = stream.len() as u32;
    raw.size = 100;
    raw.local_size = 100;
    let mut b = Builder::new();
    b.raw(raw.clone());
    for i in 1..2_000 {
        let mut twin = raw.clone();
        twin.name = format!("x{i}.bin").into_bytes();
        b.central_only(&twin, 0);
    }
    let probe = b.finish();
    let r = timed("2,000 entries sharing one 4 MB stream", || open(probe));
    damaged(r, "overlap");
}

#[test]
fn empty_deflate_blocks_cost_their_input_and_no_more() {
    // One entry, 4 MB of empty blocks, claiming 100 bytes.
    let stream = empty_blocks((4 << 20) / 5);
    let mut raw = Raw::new("x.bin", b"", Method::Deflated);
    raw.data = stream.clone();
    raw.compressed_size = stream.len() as u32;
    raw.local_compressed_size = stream.len() as u32;
    raw.size = 100;
    raw.local_size = 100;
    let mut b = Builder::new();
    b.raw(raw);
    let mut a = open(b.finish()).unwrap();
    // Refused for its shape before a byte is inflated.
    let r = timed("whole read of 4 MB of empty blocks", || a.read(0, 1 << 20));
    slow(r, "far larger than its output");
    assert_eq!(a.bytes_read(), 0);
    // The first bytes: input is charged, and the stream stops once its
    // input runs ahead of its output.
    let r = timed("prefix of 4 MB of empty blocks", || a.read_prefix(0, 8));
    slow(r, "without producing output");
    assert!(a.bytes_read() <= 4 << 10, "{}", a.bytes_read());

    // Claiming 4 MB of output makes the shape plausible; inflating then
    // stops at once (input with no output), and the input was charged
    // first.
    let stream = empty_blocks((4 << 20) / 5);
    let mut raw = Raw::new("y.bin", b"", Method::Deflated);
    raw.data = stream.clone();
    raw.compressed_size = stream.len() as u32;
    raw.local_compressed_size = stream.len() as u32;
    raw.size = 4 << 20;
    raw.local_size = 4 << 20;
    let mut b = Builder::new();
    b.raw(raw);
    let mut a = open(b.finish()).unwrap();
    let r = timed("whole read claiming 4 MB", || a.read(0, 8 << 20));
    slow(r, "without producing output");
    assert_eq!(a.bytes_read(), stream.len() as u64);
}

/// Reviewer probe z64: in a sparse 8 GB file, the locator points at a
/// zip64 record with a wrong size, and a good-looking one claiming
/// 186 million entries sits right before the locator.
#[test]
fn a_zip64_decoy_in_a_huge_file_allocates_nothing() {
    let size: u64 = 8 << 30;
    let local = Raw::new("a", b"", Method::Stored).local_header();
    let directory_at = local.len() as u64;
    let decoy_at = directory_at + 100;
    let locator_at = size - 22 - 20;
    let good_at = locator_at - 56;
    let claimed = (good_at - directory_at) / 46 - 1;
    let build = |locator_points_at: u64| {
        let mut f = Sparse::new(size);
        f.put(0, &local);
        f.put(decoy_at, &zip64_end_record_sized(999, 1, 46, directory_at));
        f.put(
            good_at,
            &zip64_end_record(claimed, claimed * 46, directory_at),
        );
        f.put(locator_at, &zip64_locator(locator_points_at));
        f.put(size - 22, &end_record(0xFFFF, u32::MAX, u32::MAX, 0));
        f
    };
    let r = timed("locator at the decoy", || {
        Archive::open(build(decoy_at), Limits::default())
    });
    damaged(r, "size does not reach its locator");
    let r = timed("locator at the claim", || {
        Archive::open(build(good_at), Limits::default())
    });
    over_limit(r, "zip entries");
    // A locator pointing at nothing.
    let r = Archive::open(build(decoy_at + 8), Limits::default());
    damaged(r, "not where its locator points");
    let r = Archive::open(build(locator_at - 10), Limits::default());
    damaged(r, "points at or past itself");
}

#[test]
fn counts_and_directory_size_are_checked_before_reading() {
    let mut b = Builder::new();
    for i in 0..20 {
        b.add(&format!("e{i}"), b"x", Method::Stored);
    }
    let bytes = b.finish();
    let tight = Limits {
        max_entries: 19,
        ..Limits::default()
    };
    over_limit(
        Archive::open(Cursor::new(bytes.clone()), tight),
        "zip entries",
    );
    let tight = Limits {
        max_directory_bytes: 100,
        ..Limits::default()
    };
    over_limit(
        Archive::open(Cursor::new(bytes.clone()), tight),
        "central directory",
    );

    // A count larger than the directory can hold.
    let mut lying = bytes.clone();
    let n = lying.len();
    lying[n - 12..n - 10].copy_from_slice(&40u16.to_le_bytes());
    lying[n - 14..n - 12].copy_from_slice(&40u16.to_le_bytes());
    damaged(open(lying), "too small for its entry count");
    // A count smaller than the directory holds.
    let mut lying = bytes;
    let n = lying.len();
    lying[n - 12..n - 10].copy_from_slice(&3u16.to_le_bytes());
    lying[n - 14..n - 12].copy_from_slice(&3u16.to_le_bytes());
    damaged(open(lying), "more than its entry count");
}

#[test]
fn data_before_the_archive_is_refused() {
    let good = zip(&[("a", b"x", Method::Stored)]);
    let mut prefixed = b"MZ self-extractor stub".to_vec();
    prefixed.extend_from_slice(&good);
    damaged(open(prefixed), "data before the archive");

    // Offsets shifted to match, so the directory lines up: the first
    // entry then does not start at 0.
    let raw = Raw::new("a", b"x", Method::Stored);
    let mut b = Builder::new();
    b.out.extend_from_slice(b"junk");
    b.raw(raw);
    damaged(open(b.finish()), "data before the first entry");
}

#[test]
fn an_entry_running_into_the_directory_is_refused() {
    let mut raw = Raw::new("a", b"xyz", Method::Stored);
    raw.compressed_size = 50;
    raw.size = 50;
    let mut b = Builder::new();
    b.raw(raw);
    damaged(open(b.finish()), "runs into the central directory");
}

#[test]
fn local_headers_must_agree_with_the_directory() {
    let read = |raw: Raw| {
        let mut b = Builder::new();
        b.raw(raw);
        open(b.finish()).unwrap().read(0, 1 << 20)
    };
    let mut r = Raw::new("xl/vbaProject.bin", b"data", Method::Stored);
    r.local_name = b"xl/decoyProject.bin".to_vec();
    damaged(read(r), "(name)");
    let mut r = Raw::new("a", b"data", Method::Stored);
    r.local_method = 8;
    damaged(read(r), "compression method");
    let mut r = Raw::new("a", b"data", Method::Stored);
    r.local_size = 3;
    damaged(read(r), "sizes");
    // Encrypted in one header only.
    let mut r = Raw::new("a", b"data", Method::Stored);
    r.local_flags = 1;
    damaged(read(r), "flags");
    // The local header's extra field claims more bytes than are there,
    // which pushes the data into the central directory.
    let mut b = Builder::new();
    b.add("a", b"data", Method::Stored);
    let mut bytes = b.finish();
    bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
    damaged(open(bytes).unwrap().read(0, 99), "runs into the next entry");
    // A header that is not where the directory says.
    let mut b = Builder::new();
    b.add("a", b"data", Method::Stored);
    let mut bytes = b.finish();
    bytes[0] = b'X';
    damaged(
        open(bytes).unwrap().read(0, 99),
        "not where the directory says",
    );
}

#[test]
fn only_stored_and_deflate_without_encryption_are_read() {
    let mut b = Builder::new();
    b.raw(Raw::new("enc", b"data", Method::Stored).flags(1));
    let mut lzma = Raw::new("lzma", b"data", Method::Stored);
    lzma.method = 14;
    lzma.local_method = 14;
    b.raw(lzma);
    let mut a = open(b.finish()).unwrap();
    assert!(a.entries()[0].is_encrypted());
    assert!(matches!(a.read(0, 99), Err(Error::Unsupported(_))));
    assert!(matches!(a.read_prefix(0, 8), Err(Error::Unsupported(_))));
    assert!(matches!(a.read(1, 99), Err(Error::Unsupported(_))));
}

#[test]
fn bombs_and_lying_sizes_stop_at_the_limits() {
    let zeros = vec![0u8; 65 << 20];
    let mut a = open(zip(&[("bomb", &zeros, Method::Deflated)])).unwrap();
    over_limit(a.read(0, 64 << 20), "one entry");
    // The prefix of a bomb is cheap.
    let head = timed("prefix of a 65 MB bomb", || a.read_prefix(0, 8)).unwrap();
    assert_eq!(head, [0; 8]);
    drop(zeros);

    // Claims 10 bytes, inflates to 64 KB: stops at 11.
    let mut raw = Raw::new("liar", &vec![1u8; 64 << 10], Method::Deflated);
    raw.size = 10;
    raw.local_size = 10;
    let mut b = Builder::new();
    b.raw(raw);
    damaged(
        open(b.finish()).unwrap().read(0, 99),
        "more bytes than it claims",
    );

    // A wrong CRC-32.
    let mut raw = Raw::new("a", b"data", Method::Deflated);
    raw.crc ^= 1;
    let mut b = Builder::new();
    b.raw(raw);
    damaged(open(b.finish()).unwrap().read(0, 99), "CRC-32");
}

#[test]
fn totals_are_budgeted_across_reads() {
    let mut b = Builder::new();
    for i in 0..10 {
        b.add(&format!("e{i}"), &[7u8; 1000], Method::Stored);
    }
    let bytes = b.finish();
    let limits = Limits {
        max_total_bytes: 3_500,
        ..Limits::default()
    };
    let mut a = Archive::open(Cursor::new(bytes.clone()), limits).unwrap();
    for i in 0..3 {
        a.read(i, 1000).unwrap();
    }
    over_limit(a.read(3, 1000), "all entries");
    let limits = Limits {
        max_total_read: 2_500,
        ..Limits::default()
    };
    let mut a = Archive::open(Cursor::new(bytes), limits).unwrap();
    a.read(0, 1000).unwrap();
    a.read(1, 1000).unwrap();
    over_limit(a.read(2, 1000), "compressed bytes read");
}

#[test]
fn split_archives_and_missing_zip64_fields_are_refused() {
    let mut bytes = zip(&[("a", b"x", Method::Stored)]);
    let n = bytes.len();
    bytes[n - 18..n - 16].copy_from_slice(&1u16.to_le_bytes());
    assert!(matches!(open(bytes), Err(Error::Unsupported(_))));

    let mut raw = Raw::new("a", b"x", Method::Stored);
    raw.size = u32::MAX;
    let mut b = Builder::new();
    b.raw(raw);
    damaged(open(b.finish()), "zip64 sizes");

    let mut bytes = zip(&[("a", b"x", Method::Stored)]);
    let n = bytes.len();
    bytes.truncate(n - 30);
    damaged(open(bytes), "no end-of-central-directory record");
    damaged(open(b"PK\x05\x06".to_vec()), "too short");
}

#[test]
fn broken_archives_never_panic() {
    let good = zip(&[
        ("a.xml", b"<a/>", Method::Deflated),
        ("b.bin", b"0123456789", Method::Stored),
    ]);
    // Flip each byte in turn and truncate at each length: errors only.
    for i in 0..good.len() {
        for flip in [0x01u8, 0x80, 0xFF] {
            let mut bytes = good.clone();
            bytes[i] ^= flip;
            if let Ok(mut a) = open(bytes) {
                for e in 0..a.entries().len() {
                    let _ = a.read(e, 1 << 20);
                    let _ = a.read_prefix(e, 8);
                }
            }
        }
        let _ = open(good[..i].to_vec());
    }
}

/// Reviewer probe `slow`: empty fixed-Huffman blocks are 10 bits each and
/// cost the inflater microseconds apiece. 4 MiB of them took 7.3 s; a
/// tail of matches made the size ratio look normal.
#[test]
fn empty_fixed_blocks_are_stopped_while_inflating() {
    let filler = 4 << 20;
    let mut b = Builder::new();
    b.raw(empty_fixed_entry("x.bin", filler, filler / 258 + 1, false));
    let mut a = open(b.finish()).unwrap();
    let r = timed("whole read: 4 MiB of empty fixed blocks first", || {
        a.read(0, 64 << 20)
    });
    slow(r, "without producing output");
    // The first bytes: refused after about 1 KiB of input.
    let r = timed("prefix: 4 MiB of empty fixed blocks first", || {
        a.read_prefix(0, 8)
    });
    slow(r, "without producing output");

    // Entries of 63 KiB filler each (the reviewer's prefix probe): each
    // is refused after about 1 KiB of input, not 63 KiB.
    let mut b = Builder::new();
    for i in 0..20 {
        b.raw(empty_fixed_entry(&format!("p{i}.bin"), 63 << 10, 1, false));
    }
    let mut a = open(b.finish()).unwrap();
    let refused = timed("20 prefixes of 63 KiB filler", || {
        (0..20).filter(|&i| a.read_prefix(i, 8).is_err()).count()
    });
    assert_eq!(refused, 20);
    assert!(a.bytes_read() < 20 * 4096, "{}", a.bytes_read());
}

/// Output first, then filler: the output earns room for that much
/// filler, so with the cap on runs without output lifted, the caller's
/// interrupt is what bounds it.
#[test]
fn the_interrupt_stops_a_long_inflate() {
    let mut b = Builder::new();
    b.raw(empty_fixed_entry(
        "x.bin",
        1 << 20,
        (1 << 20) / 258 + 64,
        true,
    ));
    let bytes = b.finish();
    let open = |bytes| {
        let limits = Limits {
            max_read_without_output: u64::MAX,
            ..Limits::default()
        };
        Archive::open(Cursor::new(bytes), limits)
    };
    let mut a = open(bytes.clone()).unwrap();
    let mut checks = 0;
    a.set_interrupt(Box::new(move || {
        checks += 1;
        checks > 50
    }));
    let r = timed("interrupted after 50 steps", || a.read(0, 64 << 20));
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    // Without the interrupt it reads, and the time is bounded by the
    // output it earned (here 1 MiB).
    let mut a = open(bytes).unwrap();
    assert!(a.read(0, 64 << 20).is_ok());
}

/// Issue 80: output, then a run of empty blocks the output has earned
/// room for, then more output. The cumulative guard lets it through; the
/// cap on input in a row without output refuses it, long before the
/// entry or the run ends.
#[test]
fn a_long_run_without_output_between_outputs_is_refused() {
    let entry = |filler: usize| {
        let mut b = Builder::new();
        b.raw(empty_fixed_entry("x.bin", filler, (1 << 20) / 258, true));
        b.finish()
    };
    // 256 KiB of filler after 1 MiB of output: within the room earned.
    let mut a = open(entry(256 << 10)).unwrap();
    let r = timed("256 KiB run of empty blocks after output", || {
        a.read(0, 64 << 20)
    });
    slow(r, "long run of input without output");
    let e = open(entry(256 << 10))
        .unwrap()
        .read(0, 64 << 20)
        .unwrap_err();
    assert!(e.is_slow_to_unpack(), "{e}");
    // Lifting the cap shows it was the cap, not the cumulative guard.
    let limits = Limits {
        max_read_without_output: u64::MAX,
        ..Limits::default()
    };
    let mut a = Archive::open(Cursor::new(entry(256 << 10)), limits).unwrap();
    assert!(a.read(0, 64 << 20).is_ok());
    // A run under the cap passes with the default limits.
    let mut a = open(entry(8 << 10)).unwrap();
    assert!(a.read(0, 64 << 20).is_ok());
    // Other refusals for wasted time say so too; damage does not.
    let mut b = Builder::new();
    b.raw(empty_fixed_entry("y.bin", 4 << 20, 1, false));
    let e = open(b.finish()).unwrap().read(0, 64 << 20).unwrap_err();
    assert!(e.is_slow_to_unpack(), "{e}");
    let mut raw = Raw::new("z", b"0123456789", Method::Deflated);
    raw.crc ^= 1;
    let mut b = Builder::new();
    b.raw(raw);
    let e = open(b.finish()).unwrap().read(0, 100).unwrap_err();
    assert!(!e.is_slow_to_unpack(), "{e}");
}

#[test]
fn real_deflate_passes_the_progress_guard() {
    // Text, incompressible bytes, and stored blocks inside deflate.
    let text = b"<c r=\"A1\" t=\"s\"><v>12</v></c>".repeat(200_000);
    let mut x: u32 = 1;
    let noise: Vec<u8> = (0..3 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    let mut stored_blocks = Vec::new();
    for chunk in noise.chunks(65_535) {
        stored_blocks.push(0u8);
        stored_blocks.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        stored_blocks.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        stored_blocks.extend_from_slice(chunk);
    }
    stored_blocks.extend_from_slice(b"\x01\x00\x00\xff\xff");
    let mut raw = Raw::new("stored-in-deflate", b"", Method::Deflated);
    raw.crc = crc32(&noise);
    raw.size = noise.len() as u32;
    raw.local_size = raw.size;
    raw.compressed_size = stored_blocks.len() as u32;
    raw.local_compressed_size = raw.compressed_size;
    raw.data = stored_blocks;
    let mut b = Builder::new();
    b.add("text.xml", &text, Method::Deflated)
        .add("noise.bin", &noise, Method::Deflated)
        .raw(raw);
    let mut a = open(b.finish()).unwrap();
    assert_eq!(a.read(0, 64 << 20).unwrap(), text);
    assert_eq!(a.read(1, 64 << 20).unwrap(), noise);
    assert_eq!(a.read(2, 64 << 20).unwrap(), noise);
    assert_eq!(a.read_prefix(1, 8).unwrap(), noise[..8]);
}

/// Valid deflate that is larger than its output: fixed-Huffman literals
/// 144 to 255 take 9 bits each. zlib-rs's fastest level grows random
/// bytes by 5.5%; a guard of 1/256 refused those files.
#[test]
fn deflate_larger_than_its_output_passes_the_progress_guard() {
    let content: Vec<u8> = (0..1u32 << 20).map(|i| 144 + (i % 112) as u8).collect();
    let mut bits = Bits::new();
    bits.put(1, 1);
    bits.put(1, 2);
    for &c in &content {
        bits.fixed(u32::from(c));
    }
    bits.fixed(256);
    let data = bits.finish();
    assert!(data.len() > content.len() * 9 / 8, "{}", data.len());
    let mut raw = Raw::new("high.bin", b"", Method::Deflated);
    raw.crc = crc32(&content);
    raw.size = content.len() as u32;
    raw.local_size = raw.size;
    raw.compressed_size = data.len() as u32;
    raw.local_compressed_size = raw.compressed_size;
    raw.data = data;
    let mut b = Builder::new();
    b.raw(raw);
    let mut a = open(b.finish()).unwrap();
    assert_eq!(a.read(0, 64 << 20).unwrap(), content);
    assert_eq!(a.read_prefix(0, 100).unwrap(), content[..100]);
}

#[test]
fn read_prefix_of_a_whole_entry_checks_it_like_read() {
    let mut raw = Raw::new("a", b"0123456789", Method::Deflated);
    raw.crc ^= 1;
    let mut b = Builder::new();
    b.raw(raw);
    let mut short = Raw::new("b", b"0123456789", Method::Deflated);
    short.size = 100;
    short.local_size = 100;
    b.raw(short);
    let mut a = open(b.finish()).unwrap();
    damaged(a.read_prefix(0, 100), "CRC-32");
    damaged(a.read_prefix(1, 100), "different size");
    // A huge n on a small entry allocates nothing big.
    let mut huge = Raw::new("c", b"x", Method::Deflated);
    huge.size = 0x3FFF_FFFF;
    huge.local_size = huge.size;
    let mut b = Builder::new();
    b.raw(huge);
    let mut a = open(b.finish()).unwrap();
    over_limit(a.read_prefix(0, 1 << 30), "one entry");
}

#[test]
fn sizes_near_the_top_of_u64_do_not_overflow() {
    let mut raw = Raw::new("a.bin", b"x", Method::Deflated);
    raw.size = u32::MAX;
    raw.central_extra = zip64_extra(&[u64::MAX]);
    let mut b = Builder::new();
    b.raw(raw);
    let limits = Limits {
        max_entry_bytes: u64::MAX,
        max_total_bytes: u64::MAX,
        ..Limits::default()
    };
    let mut a = Archive::open(Cursor::new(b.finish()), limits).unwrap();
    assert!(a.read(0, u64::MAX).is_err());
}
