//! The MS-OVBA decompressor against the spec's worked examples (section
//! 3.2) and against broken and hostile containers.

mod common;

use carrysheet_vba::{VbaError, decompress};

const MAX: usize = 1 << 20;

#[test]
fn spec_3_2_1_no_compression() {
    let packed = [
        0x01, 0x19, 0xB0, 0x00, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x00, 0x69, 0x6A,
        0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x00, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x2E,
    ];
    assert_eq!(
        decompress(&packed, MAX).unwrap(),
        b"abcdefghijklmnopqrstuv."
    );
}

#[test]
fn spec_3_2_2_normal_compression() {
    let packed = [
        0x01, 0x2F, 0xB0, 0x00, 0x23, 0x61, 0x61, 0x61, 0x62, 0x63, 0x64, 0x65, 0x82, 0x66, 0x00,
        0x70, 0x61, 0x67, 0x68, 0x69, 0x6A, 0x01, 0x38, 0x08, 0x61, 0x6B, 0x6C, 0x00, 0x20, 0x6D,
        0x6E, 0x6F, 0x70, 0x06, 0x71, 0x02, 0x70, 0x04, 0x00, 0x72, 0x73, 0x74, 0x75, 0x76, 0x10,
        0x77, 0x78, 0x79, 0x7A, 0x00, 0x2C,
    ];
    assert_eq!(
        decompress(&packed, MAX).unwrap(),
        b"#aaabcdefaaaaghijaaaaaklaaamnopqaaaaaaaaaaaarstuvwxyzaaa"
    );
}

#[test]
fn spec_3_2_3_maximum_compression() {
    let packed = [0x01, 0x03, 0xB0, 0x02, 0x61, 0x45, 0x00];
    // One literal plus a copy token of length 0x45 + 3 = 72.
    assert_eq!(decompress(&packed, MAX).unwrap(), vec![b'a'; 73]);
}

#[test]
fn raw_chunk_is_copied_as_is() {
    let data: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8).collect();
    let mut packed = vec![0x01];
    // Raw chunk: flag 0, signature 0b011, size field 0xFFF.
    packed.extend_from_slice(&0x3FFFu16.to_le_bytes());
    packed.extend_from_slice(&data);
    assert_eq!(decompress(&packed, MAX).unwrap(), data);
}

#[test]
fn literal_containers_round_trip_across_chunks() {
    let text: Vec<u8> = (0..20_000u32)
        .map(|i| b"Sub X()\r\n"[i as usize % 9])
        .collect();
    let packed = common::compress_literal(&text);
    assert_eq!(decompress(&packed, MAX).unwrap(), text);
}

fn compression_error(packed: &[u8]) -> (usize, String) {
    match decompress(packed, MAX) {
        Err(VbaError::BadCompression { offset, detail, .. }) => (offset, detail),
        other => panic!("expected a compression error, got {other:?}"),
    }
}

#[test]
fn broken_containers_say_where_and_why() {
    assert!(compression_error(&[]).1.contains("empty"));
    assert!(
        compression_error(&[0x02, 0x00])
            .1
            .contains("signature byte")
    );
    // Chunk header cut in half.
    assert_eq!(compression_error(&[0x01, 0x19]).0, 1);
    // Wrong chunk signature (0b000 instead of 0b011).
    assert!(
        compression_error(&[0x01, 0x19, 0x80, 0x00])
            .1
            .contains("0b000")
    );
    // A copy token as the first token of a chunk has nothing to copy.
    assert!(
        compression_error(&[0x01, 0x03, 0xB0, 0x01, 0x00, 0x00])
            .1
            .contains("before any data")
    );
    // Copy token pointing 2 bytes back after 1 byte (offset bits 0b0001).
    assert!(
        compression_error(&[0x01, 0x04, 0xB0, 0x02, 0x61, 0x00, 0x10])
            .1
            .contains("points 2 bytes back")
    );
    // A copy token cut off at the end of its chunk.
    assert!(
        compression_error(&[0x01, 0x02, 0xB0, 0x02, 0x61, 0x00])
            .1
            .contains("cut off")
    );
}

#[test]
fn chunk_cannot_grow_past_4096_bytes() {
    // 1 literal + copy of 4095 fills the chunk; one more literal overflows.
    let packed = [0x01, 0x04, 0xB0, 0x02, b'a', 0xFC, 0x0F, b'b'];
    let (_, detail) = compression_error(&packed);
    assert!(detail.contains("more than 4096"), "{detail}");
}

#[test]
fn decompression_bomb_stops_at_the_limit() {
    // 6 bytes per chunk expand to 4096: 60 KB would become 40 MB.
    let packed = common::bomb(10_000);
    assert_eq!(packed.len(), 60_001);
    match decompress(&packed, 1 << 20) {
        Err(VbaError::LimitExceeded { max, .. }) => assert_eq!(max, 1 << 20),
        other => panic!("expected the limit, got {:?}", other.map(|v| v.len())),
    }
    // Under the limit the same shape decompresses fine.
    assert_eq!(decompress(&common::bomb(3), MAX).unwrap().len(), 3 * 4096);
}

#[test]
fn random_containers_never_panic() {
    let mut rng = common::Rng::new(0x5EED_0001);
    let seed =
        common::compress_literal(b"Attribute VB_Name = \"Module1\"\r\nSub A()\r\nEnd Sub\r\n");
    let spec = vec![
        0x01, 0x2F, 0xB0, 0x00, 0x23, 0x61, 0x61, 0x61, 0x62, 0x63, 0x64, 0x65, 0x82, 0x66, 0x00,
        0x70, 0x61, 0x67, 0x68, 0x69, 0x6A, 0x01, 0x38, 0x08, 0x61, 0x6B, 0x6C, 0x00, 0x20, 0x6D,
        0x6E, 0x6F, 0x70, 0x06, 0x71, 0x02, 0x70, 0x04, 0x00, 0x72, 0x73, 0x74, 0x75, 0x76, 0x10,
        0x77, 0x78, 0x79, 0x7A, 0x00, 0x2C,
    ];
    for i in 0..20_000 {
        let mut bytes = if i % 2 == 0 {
            seed.clone()
        } else {
            spec.clone()
        };
        rng.mutate(&mut bytes);
        if let Ok(out) = decompress(&bytes, 64 * 1024) {
            assert!(out.len() <= 64 * 1024);
        }
    }
}
