//! `.xls` workbook streams: decrypt POI samples, re-encrypt, round trips.

mod common;

use carrysheet_crypto::{
    BiffEncryption, BiffEncryptor, CryptoError, ENCRYPTION_STREAM, Limits, Password,
    ProtectedStream, Protection, decrypt_biff, decrypt_biff_streams, detect, detect_filepass,
    detect_workbook_stream, empty_document_summary, encrypt_biff,
};
use common::{check_positions, compound, contains, records, stream, workbook};

fn open(name: &str, password: &str) -> carrysheet_crypto::DecryptedBiff {
    decrypt_biff(
        &workbook(name),
        Some(&Password::new(password)),
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn rc4_cryptoapi_sample_decrypts() {
    let enc = workbook("xls/35897-type4.xls");
    assert_eq!(
        detect_workbook_stream(&enc).unwrap(),
        Some(BiffEncryption::Rc4CryptoApi { key_bits: 128 })
    );
    let d = open("xls/35897-type4.xls", "freedom");
    assert_eq!(d.encryption, BiffEncryption::Rc4CryptoApi { key_bits: 128 });
    assert!(!d.doc_props_encrypted);
    assert!(!d.key.is_excel_default());
    // POI's TestCryptoAPI expects "Sheet1\nhello there!\n".
    assert!(contains(&d.stream, b"hello there!"));
    assert!(contains(&d.stream, b"Sheet1"));
    // FILEPASS is gone and every stream position still lands right.
    assert_eq!(detect_workbook_stream(&d.stream).unwrap(), None);
    assert!(records(&d.stream).iter().all(|r| r.1 != 0x002F));
    check_positions(&d.stream, true);
    assert_eq!(d.stream.len() + 4 + 200, enc.len());
}

#[test]
fn rc4_md5_sample_decrypts() {
    let d = open("xls/password.xls", "password");
    assert_eq!(d.encryption, BiffEncryption::Rc4);
    assert!(contains(
        &d.stream,
        b"A ZIP bomb is a variant of mail-bombing."
    ));
    check_positions(&d.stream, true);
}

#[test]
fn xor_sample_decrypts() {
    let d = open("xls/xor-encryption-abc.xls", "abc");
    assert_eq!(d.encryption, BiffEncryption::Xor);
    assert!(contains(&d.stream, b"Sheet1"));
    check_positions(&d.stream, true);
    // The three numbers 1, 2, 3 of POI's expected text, as RK or NUMBER cells.
    let cells = records(&d.stream)
        .iter()
        .filter(|r| matches!(r.1, 0x027E | 0x0203 | 0x00BD))
        .count();
    assert!(cells >= 3, "found {cells} number cells");
}

#[test]
fn wrong_or_missing_password_is_only_that() {
    for (name, right) in [
        ("xls/35897-type4.xls", "freedom"),
        ("xls/password.xls", "password"),
        ("xls/xor-encryption-abc.xls", "abc"),
    ] {
        let enc = workbook(name);
        let wrong = format!("{right}x");
        let e = decrypt_biff(&enc, Some(&Password::new(&wrong)), &Limits::default()).unwrap_err();
        assert_eq!(e, CryptoError::WrongPassword, "{name}");
        assert!(!format!("{e} {e:?}").contains(&wrong));
        let e = decrypt_biff(&enc, None, &Limits::default()).unwrap_err();
        assert_eq!(e, CryptoError::PasswordRequired, "{name}");
    }
}

#[test]
fn reencrypted_streams_round_trip() {
    for (name, pw) in [
        ("xls/35897-type4.xls", "freedom"),
        ("xls/password.xls", "password"),
        ("xls/xor-encryption-abc.xls", "abc"),
    ] {
        let plain = open(name, pw);
        let enc = plain
            .key
            .biff_encryptor()
            .unwrap()
            .workbook(&plain.stream)
            .unwrap();
        assert_eq!(
            detect_workbook_stream(&enc).unwrap(),
            Some(BiffEncryption::Rc4CryptoApi { key_bits: 128 }),
            "{name}"
        );
        // BOUNDSHEET positions stay readable in the encrypted stream.
        check_positions(&enc, false);
        assert_ne!(
            enc[enc.len() - 64..],
            plain.stream[plain.stream.len() - 64..]
        );
        let back = decrypt_biff(&enc, Some(&Password::new(pw)), &Limits::default()).unwrap();
        assert_eq!(back.stream, plain.stream, "{name}");
        // Fresh salt every time.
        assert_ne!(
            plain
                .key
                .biff_encryptor()
                .unwrap()
                .workbook(&plain.stream)
                .unwrap(),
            enc
        );
    }
}

#[test]
fn default_password_is_tried_and_kept() {
    let plain = open("xls/35897-type4.xls", "freedom").stream;
    let enc = encrypt_biff(&plain, &Password::excel_default()).unwrap();
    let d = decrypt_biff(&enc, None, &Limits::default()).unwrap();
    assert!(d.key.is_excel_default());
    assert_eq!(d.stream, plain);
    // A given password that is wrong still falls back to the default.
    let d = decrypt_biff(&enc, Some(&Password::new("nope")), &Limits::default()).unwrap();
    assert!(d.key.is_excel_default());
}

/// A tiny workbook globals stream: BOF, WRITEPROT, BOUNDSHEET, EOF, then a
/// sheet BOF and EOF.
fn tiny_stream() -> Vec<u8> {
    let rec = |ty: u16, body: &[u8]| {
        let mut v = ty.to_le_bytes().to_vec();
        v.extend_from_slice(&(body.len() as u16).to_le_bytes());
        v.extend_from_slice(body);
        v
    };
    let bof = rec(0x0809, &[0, 6, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut s = bof.clone();
    s.extend(rec(0x0086, &[]));
    let sheet_at = s.len() + 4 + 12 + 4;
    let mut bs = (sheet_at as u32).to_le_bytes().to_vec();
    bs.extend_from_slice(&[0, 0, 4, 0, b'D', b'a', b't', b'a']);
    s.extend(rec(0x0085, &bs));
    s.extend(rec(0x000A, &[]));
    assert_eq!(s.len(), sheet_at);
    s.extend(bof);
    s.extend(rec(0x0204, b"secret label body"));
    s.extend(rec(0x000A, &[]));
    s
}

#[test]
fn filepass_goes_after_writeprot_and_positions_move() {
    let plain = tiny_stream();
    let pw = Password::new("pw");
    let enc = encrypt_biff(&plain, &pw).unwrap();
    let recs = records(&enc);
    assert_eq!(
        recs.iter().map(|r| r.1).take(3).collect::<Vec<_>>(),
        vec![0x0809, 0x0086, 0x002F]
    );
    check_positions(&enc, false);
    assert!(!contains(&enc, b"secret label body"));
    let back = decrypt_biff(&enc, Some(&pw), &Limits::default()).unwrap();
    assert_eq!(back.stream, plain);
}

#[test]
fn encrypting_an_encrypted_stream_is_refused() {
    let enc = workbook("xls/35897-type4.xls");
    assert!(matches!(
        encrypt_biff(&enc, &Password::new("x")),
        Err(CryptoError::Malformed { .. })
    ));
}

#[test]
fn detect_filepass_reads_record_bodies() {
    assert_eq!(
        detect_filepass(&[0, 0, 1, 2, 3, 4]).unwrap(),
        BiffEncryption::Xor
    );
    let mut rc4 = vec![1, 0, 1, 0, 1, 0];
    rc4.extend([0u8; 48]);
    assert_eq!(detect_filepass(&rc4).unwrap(), BiffEncryption::Rc4);
    assert!(matches!(
        detect_filepass(&[1, 0, 9, 0, 9, 0]),
        Err(CryptoError::Unsupported { .. })
    ));
    assert!(matches!(
        detect_filepass(&[1, 0, 1, 0]),
        Err(CryptoError::Malformed { .. })
    ));
}

const SI: &str = "\u{5}SummaryInformation";
const DSI: &str = "\u{5}DocumentSummaryInformation";

/// The FILEPASS flags of an encrypted stream (CryptoAPI layout).
fn filepass_flags(enc: &[u8]) -> u32 {
    let (_, _, body) = records(enc).into_iter().find(|r| r.1 == 0x002F).unwrap();
    u32::from_le_bytes(body[6..10].try_into().unwrap())
}

/// A whole `.xls` with encrypted document properties, built in code: the
/// title and author survive a round trip, nothing readable is left in
/// the file, and a save keeps them encrypted under a new key.
#[test]
fn encrypted_document_properties_round_trip() {
    let pw = Password::new("props");
    let plain = tiny_stream();
    let title = b"SI: Title=Quarterly secrets".to_vec();
    let author = b"DSI: Author=Jane Example".to_vec();
    let entries = vec![
        ProtectedStream::stream(DSI, author.clone()),
        ProtectedStream::stream(SI, title.clone()),
        ProtectedStream::stream("XML", b"<xml>custom data</xml>".to_vec()),
        ProtectedStream {
            name: "MsoDataStore".into(),
            storage: true,
            data: vec![0xAB; 3000],
        },
    ];
    let enc = BiffEncryptor::new(&pw, true).unwrap();
    assert!(enc.doc_props_encrypted());
    let wb = enc.workbook(&plain).unwrap();
    // fDocProps (0x08) clear: the properties are encrypted.
    assert_eq!(filepass_flags(&wb) & 0x0C, 0x04);
    let es = enc.encryption_stream(&entries).unwrap();
    let dsi = empty_document_summary();
    let file = compound(&[
        ("/Workbook", &wb),
        (&format!("/{ENCRYPTION_STREAM}"), &es),
        (&format!("/{DSI}"), &dsi),
    ]);
    for secret in [&b"Quarterly"[..], b"Jane", b"custom data", b"secret label"] {
        assert!(!contains(&file, secret));
    }
    assert_eq!(
        detect(&file, &Limits::default()).unwrap(),
        Some(Protection::Biff(BiffEncryption::Rc4CryptoApi {
            key_bits: 128
        }))
    );

    let wb_in = stream(&file, "/Workbook");
    let es_in = stream(&file, &format!("/{ENCRYPTION_STREAM}"));
    let d = decrypt_biff_streams(&wb_in, Some(&es_in), Some(&pw), &Limits::default()).unwrap();
    assert_eq!(d.stream, plain);
    assert!(d.doc_props_encrypted && d.key.doc_props_encrypted());
    // The spec's order: other entries first, then SI, then DSI.
    let names: Vec<&str> = d
        .protected_streams
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["XML", "MsoDataStore", SI, DSI]);
    assert_eq!(d.protected_streams[2].data, title);
    assert_eq!(d.protected_streams[3].data, author);
    assert!(d.protected_streams[1].storage && !d.protected_streams[0].storage);
    assert_eq!(d.protected_streams[1].data, vec![0xAB; 3000]);

    // Save again through the key: still encrypted properties, new salt.
    let again = d.key.biff_encryptor().unwrap();
    assert!(again.doc_props_encrypted());
    let wb2 = again.workbook(&d.stream).unwrap();
    let es2 = again.encryption_stream(&d.protected_streams).unwrap();
    assert_eq!(filepass_flags(&wb2) & 0x08, 0);
    assert_ne!(es2, es);
    let d2 = decrypt_biff_streams(&wb2, Some(&es2), Some(&pw), &Limits::default()).unwrap();
    assert_eq!(d2.protected_streams, d.protected_streams);
    // A stream from another key does not decrypt into anything sensible.
    let mixed = decrypt_biff_streams(&wb2, Some(&es), Some(&pw), &Limits::default());
    assert!(mixed.is_err() || mixed.unwrap().protected_streams != d.protected_streams);

    // Files with plain properties are saved with plain properties.
    let d = decrypt_biff(
        &encrypt_biff(&plain, &pw).unwrap(),
        Some(&pw),
        &Limits::default(),
    )
    .unwrap();
    assert!(!d.doc_props_encrypted && !d.key.doc_props_encrypted());
    let plain_enc = d.key.biff_encryptor().unwrap();
    assert_eq!(
        filepass_flags(&plain_enc.workbook(&plain).unwrap()) & 0x08,
        0x08
    );
    // ... and their encryptor will not hide the summary streams.
    assert!(matches!(
        plain_enc.encryption_stream(&entries),
        Err(CryptoError::InvalidArgument { .. })
    ));
    assert!(plain_enc.encryption_stream(&entries[2..]).is_ok());
}

/// Only RC4 CryptoAPI files have an `encryption` stream.
#[test]
fn encryption_stream_needs_cryptoapi() {
    let wb = workbook("xls/xor-encryption-abc.xls");
    let e = decrypt_biff_streams(
        &wb,
        Some(&[0u8; 64]),
        Some(&Password::new("abc")),
        &Limits::default(),
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            CryptoError::Malformed {
                part: "encryption stream",
                ..
            }
        ),
        "{e:?}"
    );
}

/// The empty DocumentSummaryInformation: one property set, FMTID of the
/// document summary, only the code page.
#[test]
fn empty_document_summary_layout() {
    let d = empty_document_summary();
    assert_eq!(d.len(), 72);
    assert_eq!(&d[..2], &[0xFE, 0xFF]);
    assert_eq!(&d[28..32], &[0x02, 0xD5, 0xCD, 0xD5]);
    assert_eq!(u32::from_le_bytes(d[44..48].try_into().unwrap()), 48);
    assert_eq!(u32::from_le_bytes(d[48..52].try_into().unwrap()), 24);
}

/// A pivot cache or revision log stream: records encrypted from position 0
/// of the stream, BOF left plain; decrypts with the opened file's key.
#[test]
fn record_streams_round_trip() {
    let pw = Password::new("pivot");
    let rec = |ty: u16, body: &[u8]| {
        let mut v = ty.to_le_bytes().to_vec();
        v.extend_from_slice(&(body.len() as u16).to_le_bytes());
        v.extend_from_slice(body);
        v
    };
    let mut cache = rec(0x00C6, b"SXDB pivot cache header");
    cache.extend(rec(0x0809, &[0, 6, 0x20, 0, 0, 0, 0, 0]));
    for i in 0..300u16 {
        cache.extend(rec(
            0x00C8,
            format!("item {i} with private text").as_bytes(),
        ));
    }
    cache.extend(rec(0x000A, &[]));
    let enc = BiffEncryptor::new(&pw, false).unwrap();
    let wb = enc.workbook(&tiny_stream()).unwrap();
    let cache_enc = enc.record_stream(&cache).unwrap();
    assert_eq!(cache_enc.len(), cache.len());
    assert!(!contains(&cache_enc, b"private text"));
    assert!(contains(
        &cache_enc,
        &rec(0x0809, &[0, 6, 0x20, 0, 0, 0, 0, 0])
    ));
    let d = decrypt_biff(&wb, Some(&pw), &Limits::default()).unwrap();
    assert_eq!(d.decrypt_record_stream(&cache_enc).unwrap(), cache);
    // A record stream that runs past its end is an error, not a panic.
    assert!(
        d.decrypt_record_stream(&cache_enc[..cache_enc.len() - 10])
            .is_err()
    );
}

/// Excel 5.0/95 (BIFF5) workbooks are refused with a clear error.
#[test]
fn biff5_is_refused() {
    let mut biff5 = tiny_stream();
    biff5[4..6].copy_from_slice(&0x0500u16.to_le_bytes());
    let e = encrypt_biff(&biff5, &Password::new("x")).unwrap_err();
    assert!(matches!(e, CryptoError::InvalidArgument { .. }), "{e:?}");
    assert!(e.to_string().contains("BIFF5"));
    assert_eq!(detect_workbook_stream(&biff5).unwrap(), None);
    // A BIFF5 FILEPASS (XOR, no type field) is reported, not misread.
    let mut enc = biff5[..20].to_vec();
    enc.extend([0x2F, 0x00, 4, 0, 0x4A, 0x51, 0x1A, 0xCC]);
    enc.extend_from_slice(&biff5[20..]);
    for e in [
        detect_workbook_stream(&enc).unwrap_err(),
        decrypt_biff(&enc, Some(&Password::new("abc")), &Limits::default()).unwrap_err(),
    ] {
        assert!(matches!(e, CryptoError::Unsupported { .. }), "{e:?}");
        assert!(e.to_string().contains("BIFF5"));
    }
}

/// Review L-a: properties are never dropped silently. `decrypt_biff` on a
/// props-encrypted workbook points to `decrypt_biff_streams`, and a
/// props-encrypted encryptor refuses an empty `encryption` stream.
#[test]
fn encrypted_properties_cannot_be_lost_silently() {
    let pw = Password::new("props");
    let enc = BiffEncryptor::new(&pw, true).unwrap();
    let wb = enc.workbook(&tiny_stream()).unwrap();
    let e = decrypt_biff(&wb, Some(&pw), &Limits::default()).unwrap_err();
    assert!(matches!(e, CryptoError::InvalidArgument { .. }), "{e:?}");
    assert!(e.to_string().contains("decrypt_biff_streams"), "{e}");
    assert!(matches!(
        enc.encryption_stream(&[]),
        Err(CryptoError::InvalidArgument { .. })
    ));
    // Opened without its encryption stream, the key still will not save
    // an empty one.
    let d = decrypt_biff_streams(&wb, None, Some(&pw), &Limits::default()).unwrap();
    assert!(d.protected_streams.is_empty());
    assert!(matches!(
        d.key
            .biff_encryptor()
            .unwrap()
            .encryption_stream(&d.protected_streams),
        Err(CryptoError::InvalidArgument { .. })
    ));
    // Plain properties: an empty list is fine.
    assert!(
        BiffEncryptor::new(&pw, false)
            .unwrap()
            .encryption_stream(&[])
            .is_ok()
    );
}
