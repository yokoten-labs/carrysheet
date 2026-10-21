//! Detection without a password, and the Rights Management explanation.

mod common;

use carrysheet_crypto::{BiffEncryption, CryptoError, Limits, Protection, decrypt_ooxml, detect};
use common::{compound, data, utf16};

fn kind(name: &str) -> Option<Protection> {
    detect(&data(name), &Limits::default()).unwrap()
}

#[test]
fn samples_are_recognised() {
    match kind("xlsx/60320-protected.xlsx") {
        Some(Protection::Agile(a)) => {
            assert_eq!(
                (a.cipher.as_str(), a.key_bits, a.hash.as_str()),
                ("AES", 128, "SHA1")
            );
            assert!(a.password && !a.certificate);
        }
        other => panic!("{other:?}"),
    }
    match kind("xlsx/protected_sha512.xlsx") {
        Some(Protection::Agile(a)) => assert_eq!((a.key_bits, a.hash.as_str()), (256, "SHA512")),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        kind("xlsx/protect.xlsx"),
        Some(Protection::Standard { key_bits: 128 })
    );
    assert_eq!(
        kind("xls/password.xls"),
        Some(Protection::Biff(BiffEncryption::Rc4))
    );
    assert_eq!(
        kind("xls/xor-encryption-abc.xls"),
        Some(Protection::Biff(BiffEncryption::Xor))
    );
    assert_eq!(
        kind("xls/35897-type4.xls"),
        Some(Protection::Biff(BiffEncryption::Rc4CryptoApi {
            key_bits: 128
        }))
    );
    for name in ["xlsx/protect.xlsx", "xls/password.xls"] {
        assert!(kind(name).unwrap().password_can_open());
    }
}

#[test]
fn unprotected_inputs_are_none() {
    let limits = Limits::default();
    assert_eq!(detect(b"PK\x03\x04 a zip", &limits).unwrap(), None);
    assert_eq!(detect(b"", &limits).unwrap(), None);
    // A plain .xls: the decrypted sample in a fresh compound file.
    let plain = carrysheet_crypto::decrypt_biff(
        &common::workbook("xls/35897-type4.xls"),
        Some(&carrysheet_crypto::Password::new("freedom")),
        &limits,
    )
    .unwrap()
    .stream;
    let xls = compound(&[("/Workbook", &plain)]);
    assert_eq!(detect(&xls, &limits).unwrap(), None);
}

fn rights_managed(mark: &str) -> Vec<u8> {
    match mark {
        "map" => {
            let mut map = vec![8, 0, 0, 0, 1, 0, 0, 0];
            map.extend(utf16("EncryptedPackage DRMEncryptedDataSpace"));
            compound(&[
                ("/\u{6}DataSpaces/DataSpaceMap", &map),
                ("/EncryptedPackage", &[0u8; 64]),
            ])
        }
        "content" => compound(&[("/\u{9}DRMContent", &[0u8; 64]), ("/Workbook", &[0u8; 8])]),
        _ => compound(&[
            (
                "/\u{6}DataSpaces/TransformInfo/DRMEncryptedTransform/\u{6}Primary",
                &[0u8; 8],
            ),
            ("/EncryptedPackage", &[0u8; 64]),
        ]),
    }
}

#[test]
fn rights_management_is_detected_and_explained() {
    for mark in ["map", "content", "transform"] {
        let file = rights_managed(mark);
        assert_eq!(
            detect(&file, &Limits::default()).unwrap(),
            Some(Protection::RightsManagement),
            "{mark}"
        );
        let e = decrypt_ooxml(&file, None, &Limits::default()).unwrap_err();
        assert_eq!(e, CryptoError::RightsManagement);
    }
    let e = CryptoError::RightsManagement;
    assert!(e.what().contains("sensitivity label"));
    assert!(e.why().contains("signed in"));
    assert!(e.fix().contains("owner"));
    assert!(!Protection::RightsManagement.password_can_open());
}

#[test]
fn extensible_and_certificate_only_are_named() {
    let mut info = vec![4, 0, 3, 0];
    info.extend([0u8; 16]);
    let file = compound(&[
        ("/EncryptionInfo", &info),
        ("/EncryptedPackage", &[0u8; 16]),
    ]);
    assert_eq!(
        detect(&file, &Limits::default()).unwrap(),
        Some(Protection::Extensible)
    );
    assert!(matches!(
        decrypt_ooxml(&file, None, &Limits::default()),
        Err(CryptoError::Unsupported { .. })
    ));

    let xml = r#"<?xml version="1.0"?><encryption><keyData saltSize="16" blockSize="16" keyBits="256" hashSize="64" cipherAlgorithm="AES" cipherChaining="ChainingModeCBC" hashAlgorithm="SHA512" saltValue="AAAAAAAAAAAAAAAAAAAAAA=="/><keyEncryptors><keyEncryptor uri="http://schemas.microsoft.com/office/2006/keyEncryptor/certificate"><c:encryptedKey/></keyEncryptor></keyEncryptors></encryption>"#;
    let mut info = vec![4, 0, 4, 0, 0x40, 0, 0, 0];
    info.extend_from_slice(xml.as_bytes());
    let file = compound(&[
        ("/EncryptionInfo", &info),
        ("/EncryptedPackage", &[0u8; 16]),
    ]);
    match detect(&file, &Limits::default()).unwrap() {
        Some(Protection::Agile(a)) => assert!(a.certificate && !a.password),
        other => panic!("{other:?}"),
    }
    let e = decrypt_ooxml(&file, None, &Limits::default()).unwrap_err();
    assert!(e.to_string().contains("certificate"));
}

#[test]
fn detection_reads_a_workbook_whose_size_is_too_large() {
    // An RC4 workbook padded past the 64 KB detection reads, in a fresh
    // file whose directory then says the stream is 1 GB.
    let mut wb = common::workbook("xls/password.xls");
    wb.resize(80_000, 0);
    let mut bytes = compound(&[("/Workbook", &wb)]);
    let cf = carrysheet_cfb::CompoundFile::open(&bytes).unwrap();
    let id = cf.entry("/Workbook").unwrap().id() as usize;
    let ss = 512;
    let dir = (u32::from_le_bytes(bytes[48..52].try_into().unwrap()) as usize + 1) * ss;
    let at = dir + id * 128 + 120;
    bytes[at..at + 4].copy_from_slice(&0x4000_0000u32.to_le_bytes());
    assert_eq!(
        detect(&bytes, &Limits::default()).unwrap(),
        Some(Protection::Biff(BiffEncryption::Rc4))
    );
}
