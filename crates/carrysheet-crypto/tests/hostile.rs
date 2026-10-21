//! Hostile input: everything here must end in an error, quickly, with no
//! panic and no allocation beyond the input's size.

mod common;

use std::time::{Duration, Instant};

use carrysheet_crypto::{
    BiffEncryptor, CryptoError, Limits, Password, ProtectedStream, decrypt_biff,
    decrypt_biff_streams, decrypt_ooxml, detect, detect_workbook_stream, encrypt_biff,
};
use common::{compound, data, replace_stream, stream, workbook};

const QUICK: Duration = Duration::from_secs(3);

/// An agile file around the given `p:encryptedKey` attributes.
fn agile(key_data: &str, encrypted_key: &str) -> Vec<u8> {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><encryption xmlns="http://schemas.microsoft.com/office/2006/encryption" xmlns:p="http://schemas.microsoft.com/office/2006/keyEncryptor/password"><keyData {key_data}/><keyEncryptors><keyEncryptor uri="http://schemas.microsoft.com/office/2006/keyEncryptor/password"><p:encryptedKey {encrypted_key}/></keyEncryptor></keyEncryptors></encryption>"#
    );
    agile_xml(&xml)
}

fn agile_xml(xml: &str) -> Vec<u8> {
    let mut info = vec![4, 0, 4, 0, 0x40, 0, 0, 0];
    info.extend_from_slice(xml.as_bytes());
    compound(&[
        ("/EncryptionInfo", &info),
        ("/EncryptedPackage", &[0u8; 64]),
    ])
}

const PARAMS: &str = r#"saltSize="16" blockSize="16" keyBits="256" hashSize="64" cipherAlgorithm="AES" cipherChaining="ChainingModeCBC" hashAlgorithm="SHA512" saltValue="AAAAAAAAAAAAAAAAAAAAAA==""#;

fn key(spin: &str, params: &str) -> String {
    let b16 = "AAAAAAAAAAAAAAAAAAAAAA==";
    let b32 = "A".repeat(43) + "=";
    let b64 = "A".repeat(86) + "==";
    format!(
        r#"spinCount="{spin}" {params} encryptedVerifierHashInput="{b16}" encryptedVerifierHashValue="{b64}" encryptedKeyValue="{b32}""#
    )
}

fn open(file: &[u8], limits: &Limits) -> CryptoError {
    let start = Instant::now();
    let e = decrypt_ooxml(file, Some(&Password::new("x")), limits).unwrap_err();
    assert!(start.elapsed() < QUICK, "took {:?}", start.elapsed());
    e
}

#[test]
fn spin_count_is_capped() {
    let file = agile(PARAMS, &key("4000000000", PARAMS));
    assert_eq!(
        open(&file, &Limits::default()),
        CryptoError::LimitExceeded {
            limit: "spin count",
            max: 10_000_000
        }
    );
    let file = agile(PARAMS, &key("200", PARAMS));
    let limits = Limits {
        max_spin_count: 100,
        ..Limits::default()
    };
    assert!(matches!(
        open(&file, &limits),
        CryptoError::LimitExceeded { .. }
    ));
    // Under the cap, the made-up file gets past the spin check and stops
    // at its missing dataIntegrity, before any key stretching.
    assert_eq!(
        open(&file, &Limits::default()),
        CryptoError::IntegrityMissing
    );
}

#[test]
fn absurd_parameters_are_refused() {
    for (what, params) in [
        (
            "key bits",
            PARAMS.replace(r#"keyBits="256""#, r#"keyBits="99999""#),
        ),
        (
            "block size",
            PARAMS.replace(r#"blockSize="16""#, r#"blockSize="4096""#),
        ),
        (
            "hash size",
            PARAMS.replace(r#"hashSize="64""#, r#"hashSize="7""#),
        ),
        (
            "salt size",
            PARAMS.replace(r#"saltSize="16""#, r#"saltSize="999""#),
        ),
        (
            "number",
            PARAMS.replace(r#"keyBits="256""#, r#"keyBits="-1""#),
        ),
    ] {
        let file = agile(&params, &key("1", PARAMS));
        assert!(
            matches!(
                open(&file, &Limits::default()),
                CryptoError::Malformed { .. }
            ),
            "{what}"
        );
    }
    for (what, params) in [
        (
            "cipher",
            PARAMS.replace(r#"cipherAlgorithm="AES""#, r#"cipherAlgorithm="3DES""#),
        ),
        (
            "hash",
            PARAMS.replace(r#"hashAlgorithm="SHA512""#, r#"hashAlgorithm="WHIRLPOOL""#),
        ),
        (
            "chaining",
            PARAMS.replace("ChainingModeCBC", "ChainingModeCFB"),
        ),
    ] {
        let file = agile(&params, &key("1", PARAMS));
        assert!(
            matches!(
                open(&file, &Limits::default()),
                CryptoError::Unsupported { .. }
            ),
            "{what}"
        );
    }
}

#[test]
fn huge_or_broken_xml_is_bounded() {
    // A 500 KB base64 salt: refused after 64 decoded bytes.
    let big = PARAMS.replace("AAAAAAAAAAAAAAAAAAAAAA==", &"A".repeat(500_000));
    assert!(matches!(
        open(&agile(&big, &key("1", PARAMS)), &Limits::default()),
        CryptoError::Malformed { .. }
    ));

    // 50,000 elements: stopped by the event budget.
    let many = format!("<encryption>{}</encryption>", "<x/>".repeat(50_000));
    assert!(matches!(
        open(&agile_xml(&many), &Limits::default()),
        CryptoError::LimitExceeded { .. }
    ));

    // Entities from a DTD are never expanded.
    let bomb = r#"<?xml version="1.0"?><!DOCTYPE e [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">]><encryption><keyData saltValue="&b;"/></encryption>"#;
    assert!(matches!(
        open(&agile_xml(bomb), &Limits::default()),
        CryptoError::Malformed { .. }
    ));

    // Too many attributes on one element.
    let attrs: String = (0..100).map(|i| format!(r#" a{i}="1""#)).collect();
    let xml = format!("<encryption><keyData{attrs}/></encryption>");
    assert!(matches!(
        open(&agile_xml(&xml), &Limits::default()),
        CryptoError::Malformed { .. }
    ));

    for xml in [
        "",
        "<encryption>",
        "<encryption><keyData saltValue=\"\u{0}\"/>",
        "\u{feff}<a></b>",
        "not xml at all",
    ] {
        let e = open(&agile_xml(xml), &Limits::default());
        assert!(matches!(e, CryptoError::Malformed { .. }), "{xml:?}: {e:?}");
    }

    // An EncryptionInfo stream over the limit is not read at all.
    let limits = Limits {
        max_info_bytes: 100,
        ..Limits::default()
    };
    let e = open(&agile(PARAMS, &key("1", PARAMS)), &limits);
    assert!(matches!(e, CryptoError::LimitExceeded { .. }), "{e:?}");
}

#[test]
fn declared_package_size_must_be_backed_by_data() {
    let file = data("xlsx/protect.xlsx");
    let mut pkg = stream(&file, "/EncryptedPackage");
    pkg[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    let bad = replace_stream(&file, "/EncryptedPackage", &pkg);
    let start = Instant::now();
    let e = decrypt_ooxml(&bad, None, &Limits::default()).unwrap_err();
    assert!(
        matches!(
            e,
            CryptoError::Malformed {
                part: "EncryptedPackage",
                ..
            }
        ),
        "{e:?}"
    );
    assert!(start.elapsed() < QUICK);

    let bad = replace_stream(&file, "/EncryptedPackage", &pkg[..5]);
    assert!(matches!(
        decrypt_ooxml(&bad, None, &Limits::default()),
        Err(CryptoError::Malformed { .. })
    ));
}

#[test]
fn input_size_limit() {
    let limits = Limits {
        max_input_bytes: 1000,
        ..Limits::default()
    };
    let file = data("xlsx/protect.xlsx");
    assert!(matches!(
        detect(&file, &limits),
        Err(CryptoError::LimitExceeded { .. })
    ));
    assert!(matches!(
        decrypt_ooxml(&file, None, &limits),
        Err(CryptoError::LimitExceeded { .. })
    ));
    let wb = workbook("xls/password.xls");
    assert!(matches!(
        decrypt_biff(&wb, None, &limits),
        Err(CryptoError::LimitExceeded { .. })
    ));
}

#[test]
fn cut_and_flipped_files_never_panic() {
    let limits = Limits::default();
    for name in [
        "xlsx/60320-protected.xlsx",
        "xlsx/protect.xlsx",
        "xls/password.xls",
        "xls/35897-type4.xls",
        "xls/xor-encryption-abc.xls",
    ] {
        let file = data(name);
        for cut in [0, 8, 100, 511, 512, 1000, file.len() / 2, file.len() - 1] {
            let _ = detect(&file[..cut], &limits);
            let _ = decrypt_ooxml(&file[..cut], None, &limits);
        }
        for at in (0..file.len()).step_by(97) {
            let mut f = file.clone();
            f[at] ^= 0x5A;
            let _ = detect(&f, &limits);
        }
    }
    // The standard sample decrypts in milliseconds: flip every byte of
    // its EncryptionInfo header.
    let file = data("xlsx/protect.xlsx");
    let info = stream(&file, "/EncryptionInfo");
    for at in 0..48.min(info.len()) {
        let mut i = info.clone();
        i[at] ^= 0xFF;
        let f = replace_stream(&file, "/EncryptionInfo", &i);
        let _ = decrypt_ooxml(&f, None, &limits).map_err(|e| e.to_string());
    }
}

#[test]
fn cut_and_flipped_workbook_streams_never_panic() {
    let limits = Limits::default();
    for (name, pw) in [
        ("xls/password.xls", "password"),
        ("xls/35897-type4.xls", "freedom"),
        ("xls/xor-encryption-abc.xls", "abc"),
    ] {
        let wb = workbook(name);
        let pw = Password::new(pw);
        for cut in (0..300).chain([wb.len() / 2, wb.len() - 1]) {
            let _ = detect_workbook_stream(&wb[..cut]);
            let _ = decrypt_biff(&wb[..cut], Some(&pw), &limits);
        }
        for at in (0..260).chain((260..wb.len()).step_by(61)) {
            let mut w = wb.clone();
            w[at] ^= 0xA5;
            let _ = decrypt_biff(&w, Some(&pw), &limits);
        }
    }
}

/// A BIFF8 BOF body (version 0x0600, workbook globals).
const BIFF8_BOF: [u8; 16] = [0, 6, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

fn rec(ty: u16, body: &[u8]) -> Vec<u8> {
    let mut v = ty.to_le_bytes().to_vec();
    v.extend_from_slice(&(body.len() as u16).to_le_bytes());
    v.extend_from_slice(body);
    v
}

#[test]
fn many_empty_records_cost_no_more_than_their_bytes() {
    // 1 MB of empty BOUNDSHEET, INDEX and EXTSST records after an XOR
    // FILEPASS (key and verifier of "abc").
    let mut wb = rec(0x0809, &BIFF8_BOF);
    wb.extend(rec(0x002F, &[0, 0, 0x4A, 0x51, 0x1A, 0xCC]));
    for i in 0..(1 << 20) / 12 {
        wb.extend(rec([0x0085, 0x020B, 0x00FF][i % 3], &[]));
    }
    let start = Instant::now();
    let d = decrypt_biff(&wb, Some(&Password::new("abc")), &Limits::default()).unwrap();
    assert!(start.elapsed() < QUICK, "took {:?}", start.elapsed());
    assert_eq!(d.stream.len(), wb.len() - 10);
}

#[test]
fn broken_filepass_records() {
    let bof = rec(0x0809, &BIFF8_BOF);
    let cases: Vec<Vec<u8>> = vec![
        // CryptoAPI with a 4 GB header size.
        [&[1u8, 0, 4, 0, 2, 0, 4, 0, 0, 0][..], &[0xFF; 4]].concat(),
        // CryptoAPI header with a 4096-bit RC4 key.
        {
            let mut b = vec![1u8, 0, 4, 0, 2, 0, 4, 0, 0, 0, 32, 0, 0, 0];
            for v in [4u32, 0, 0x6801, 0x8004, 4096, 1, 0, 0] {
                b.extend(v.to_le_bytes());
            }
            b.extend(16u32.to_le_bytes());
            b.extend([0u8; 32]);
            b.extend(20u32.to_le_bytes());
            b.extend([0u8; 20]);
            b
        },
        // RC4 cut short.
        vec![1, 0, 1, 0, 1, 0, 9, 9],
        // XOR cut short.
        vec![0, 0, 1],
    ];
    for body in cases {
        let mut wb = bof.clone();
        wb.extend(rec(0x002F, &body));
        wb.extend(rec(0x000A, &[]));
        let e = decrypt_biff(&wb, Some(&Password::new("x")), &Limits::default()).unwrap_err();
        assert!(matches!(e, CryptoError::Malformed { .. }), "{e:?}");
    }
    // A record that says it is longer than the stream.
    let mut wb = bof.clone();
    wb.extend(rec(0x002F, &[0, 0, 0x4A, 0x51, 0x1A, 0xCC]));
    wb.extend([0x85, 0x00, 0xFF, 0xFF, 1, 2]);
    assert!(matches!(
        decrypt_biff(&wb, Some(&Password::new("abc")), &Limits::default()),
        Err(CryptoError::Malformed { .. })
    ));
    // Not BIFF at all.
    assert!(matches!(
        detect_workbook_stream(b"hello world!"),
        Err(CryptoError::Malformed { .. })
    ));
    assert_eq!(
        decrypt_biff(&bof, None, &Limits::default()).unwrap_err(),
        CryptoError::NotEncrypted
    );
}

/// Review probe "rc4cost": 16 MB of empty records (4 bytes each) cost
/// about what 16 MB of full records cost, both ways.
#[test]
fn rc4_empty_records_are_cheap() {
    let pw = Password::new("cost");
    let mut empty = rec(0x0809, &BIFF8_BOF);
    while empty.len() < 16 << 20 {
        empty.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    }
    let mut full = rec(0x0809, &BIFF8_BOF);
    while full.len() < 16 << 20 {
        full.extend(rec(0x0001, &[0x5A; 8000]));
    }
    let mut took = Vec::new();
    for plain in [&empty, &full] {
        let start = Instant::now();
        let enc = encrypt_biff(plain, &pw).unwrap();
        let d = decrypt_biff(&enc, Some(&pw), &Limits::default()).unwrap();
        took.push(start.elapsed());
        assert!(d.stream == *plain);
    }
    // Debug builds: a few seconds at most for 4 million records; the
    // bound catches a per-record setup cost like the old 1 KB buffer.
    assert!(
        took[0] < Duration::from_secs(8),
        "empty records took {:?}",
        took[0]
    );
    assert!(took[0] < took[1] * 4 + Duration::from_secs(1), "{took:?}");
}

/// The block 0 keystream of `enc`, learned from an entry of zeros (RC4 is
/// XOR, so a test can write any `encryption` stream with it).
fn keystream0(enc: &BiffEncryptor, len: usize) -> Vec<u8> {
    let es = enc
        .encryption_stream(&[ProtectedStream::stream("X", vec![0; len])])
        .unwrap();
    es[8..8 + len].to_vec()
}

fn xor(data: &[u8], ks: &[u8]) -> Vec<u8> {
    data.iter().zip(ks).map(|(a, b)| a ^ b).collect()
}

/// An `encryption` stream: header, `data` (block 0), descriptor array.
fn encryption_stream(ks: &[u8], data: &[u8], array: &[u8]) -> Vec<u8> {
    let offset = 8 + data.len() as u32;
    let mut head = offset.to_le_bytes().to_vec();
    head.extend((array.len() as u32).to_le_bytes());
    let mut out = xor(&head, ks);
    out.extend(xor(data, ks));
    out.extend(xor(array, ks));
    out
}

fn descriptor(offset: u32, size: u32, name: &[u16]) -> Vec<u8> {
    let mut d = offset.to_le_bytes().to_vec();
    d.extend(size.to_le_bytes());
    d.extend(0u16.to_le_bytes()); // block
    d.push(name.len() as u8);
    d.push(1); // stream
    d.extend(0u32.to_le_bytes());
    for u in name {
        d.extend(u.to_le_bytes());
    }
    d.extend(0u16.to_le_bytes());
    d
}

/// The `encryption` stream is budgeted by what it produces: entry count,
/// overlap and sizes are checked against the stream, quickly.
#[test]
fn hostile_encryption_streams_are_bounded() {
    let pw = Password::new("es");
    let enc = BiffEncryptor::new(&pw, true).unwrap();
    let mut globals = rec(0x0809, &BIFF8_BOF);
    globals.extend(rec(0x000A, &[]));
    let wb = enc.workbook(&globals).unwrap();
    let ks = keystream0(&enc, 64 << 10);
    let data = vec![0x41u8; 4000];
    let name = [u16::from(b'A')];
    let array = |count: u32, descs: &[Vec<u8>]| {
        let mut a = count.to_le_bytes().to_vec();
        for d in descs {
            a.extend(d);
        }
        a
    };
    let one = descriptor(8, 4000, &name);
    // The good case first, so the hostile ones fail for their own reason.
    let good = encryption_stream(&ks, &data, &array(1, std::slice::from_ref(&one)));
    let d = decrypt_biff_streams(&wb, Some(&good), Some(&pw), &Limits::default()).unwrap();
    assert_eq!(d.protected_streams[0].data, data);

    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "huge count",
            encryption_stream(&ks, &data, &array(u32::MAX, std::slice::from_ref(&one))),
        ),
        // 255 entries that all point at the same 4000 bytes: 1 MB out of
        // a 9 KB stream.
        (
            "overlap",
            encryption_stream(&ks, &data, &array(255, &vec![one.clone(); 255])),
        ),
        ("over 256 entries", {
            let tiny = descriptor(8, 0, &name);
            encryption_stream(&ks, &data, &array(300, &vec![tiny; 300]))
        }),
        (
            "entry past the data",
            encryption_stream(&ks, &data, &array(1, &[descriptor(8, 5000, &name)])),
        ),
        (
            "entry in the header",
            encryption_stream(&ks, &data, &array(1, &[descriptor(0, 8, &name)])),
        ),
        (
            "empty name",
            encryption_stream(&ks, &data, &array(1, &[descriptor(8, 1, &[])])),
        ),
        (
            "bad UTF-16 name",
            encryption_stream(&ks, &data, &array(1, &[descriptor(8, 1, &[0xD800])])),
        ),
        ("array past the end", {
            let mut s = encryption_stream(&ks, &data, &array(1, std::slice::from_ref(&one)));
            s.truncate(s.len() - 1);
            s
        }),
        ("short header", good[..5].to_vec()),
    ];
    for (what, es) in cases {
        let start = Instant::now();
        let e = decrypt_biff_streams(&wb, Some(&es), Some(&pw), &Limits::default()).unwrap_err();
        assert!(
            start.elapsed() < QUICK,
            "{what}: took {:?}",
            start.elapsed()
        );
        assert!(
            matches!(
                e,
                CryptoError::Malformed {
                    part: "encryption stream",
                    ..
                }
            ),
            "{what}: {e:?}"
        );
    }
    // The input limit counts the encryption stream too.
    let limits = Limits {
        max_input_bytes: wb.len() + 10,
        ..Limits::default()
    };
    assert!(matches!(
        decrypt_biff_streams(&wb, Some(&good), Some(&pw), &limits),
        Err(CryptoError::LimitExceeded { .. })
    ));
}
