//! Encrypted `.xlsx` family files: an OLE2 compound file holding
//! `EncryptionInfo`, `EncryptedPackage` (the zip, encrypted) and the
//! `\x06DataSpaces` storage that names the transform (MS-OFFCRYPTO 2.1,
//! 2.3.4).

mod agile;
mod standard;

pub(crate) use standard::{CryptoApiHeader, F_CRYPTOAPI, F_DOCPROPS, Verifier};

use carrysheet_cfb::{Builder, CfbError, Version};

use crate::detect::{
    OLE2_MAGIC, Protection, check_input, is_rights_managed, open, read_stream, read_stream_with,
};
use crate::error::CryptoError;
use crate::util::Reader;
use crate::{Limits, Password, SaveKey};

/// A decrypted `.xlsx`, `.xlsm` or `.xlsb` package.
#[derive(Debug)]
pub struct DecryptedPackage {
    /// The zip bytes of the package, as they were before encryption.
    pub package: Vec<u8>,
    /// How the file was protected.
    pub protection: Protection,
    /// Whether the package's integrity check (agile HMAC) matched. Agile
    /// files must have one (a file without it is refused with
    /// [`CryptoError::IntegrityMissing`]); standard encryption has none, so
    /// this is `false` there.
    pub integrity_checked: bool,
    /// What a save needs to keep the file protected.
    pub key: SaveKey,
}

/// Settings for [`encrypt_ooxml`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncryptOptions {
    /// Key-stretching rounds: 1 to 10,000,000. Excel writes 100,000, the
    /// default here and the least a [`SaveKey`](crate::SaveKey) writes;
    /// fewer only make sense in tests.
    pub spin_count: u32,
}

impl Default for EncryptOptions {
    fn default() -> Self {
        EncryptOptions {
            spin_count: 100_000,
        }
    }
}

/// The parsed `EncryptionInfo` stream.
pub(crate) enum Info {
    Agile(Box<agile::AgileInfo>),
    Standard(standard::StandardInfo),
    Extensible,
}

pub(crate) fn parse_info(bytes: &[u8], limits: &Limits) -> Result<Info, CryptoError> {
    let mut r = Reader::new(bytes, "EncryptionInfo");
    let major = r.u16("the major version")?;
    let minor = r.u16("the minor version")?;
    match (major, minor) {
        (4, 4) => {
            let _reserved = r.u32("the reserved field")?;
            Ok(Info::Agile(Box::new(agile::parse(r.rest(), limits)?)))
        }
        (2..=4, 2) => Ok(Info::Standard(standard::parse(&mut r)?)),
        (3 | 4, 3) => Ok(Info::Extensible),
        _ => Err(CryptoError::unsupported(format!(
            "an unknown encryption version ({major}.{minor})"
        ))),
    }
}

pub(crate) fn describe(bytes: &[u8], limits: &Limits) -> Result<Protection, CryptoError> {
    Ok(match parse_info(bytes, limits)? {
        Info::Agile(a) => Protection::Agile(a.summary()),
        Info::Standard(s) => s.protection(),
        Info::Extensible => Protection::Extensible,
    })
}

/// Decrypts a password-protected `.xlsx`, `.xlsm` or `.xlsb` file.
///
/// Takes the whole file (the compound file). Tries `password` first,
/// then Excel's default password ([`DEFAULT_PASSWORD`](crate::DEFAULT_PASSWORD)),
/// so write-protected files open with `None`. The agile integrity check
/// (HMAC) is verified before anything is decrypted.
pub fn decrypt_ooxml(
    file: &[u8],
    password: Option<&Password>,
    limits: &Limits,
) -> Result<DecryptedPackage, CryptoError> {
    if !file.starts_with(&OLE2_MAGIC) {
        return Err(if file.starts_with(b"PK") {
            CryptoError::NotEncrypted
        } else {
            CryptoError::NotCompoundFile
        });
    }
    check_input(file, limits)?;
    let cf = open(file)?;
    if is_rights_managed(&cf, limits)? {
        return Err(CryptoError::RightsManagement);
    }
    if !cf.is_stream("/EncryptionInfo") {
        if cf.is_stream("/Workbook") {
            return Err(CryptoError::malformed(
                "EncryptionInfo",
                "this is an .xls file, whose Workbook stream is decrypted with `decrypt_biff`",
            ));
        }
        return Err(CryptoError::NotEncrypted);
    }
    let info = read_stream(
        &cf,
        "/EncryptionInfo",
        limits.max_info_bytes,
        "EncryptionInfo",
    )?;
    if !cf.is_stream("/EncryptedPackage") {
        return Err(CryptoError::malformed(
            "EncryptedPackage",
            "the file has EncryptionInfo but no EncryptedPackage stream",
        ));
    }
    let package = read_stream_with(
        &cf,
        "/EncryptedPackage",
        limits.max_input_bytes,
        "EncryptedPackage",
        true,
    )?;
    match parse_info(&info, limits)? {
        Info::Agile(a) => {
            let protection = Protection::Agile(a.summary());
            let ((package, integrity_checked), key) = a.decrypt(&package, password, limits)?;
            Ok(DecryptedPackage {
                package,
                protection,
                integrity_checked,
                key,
            })
        }
        Info::Standard(s) => {
            let protection = s.protection();
            let (package, key) = s.decrypt(&package, password)?;
            Ok(DecryptedPackage {
                package,
                protection,
                integrity_checked: false,
                key,
            })
        }
        Info::Extensible => Err(CryptoError::unsupported("extensible encryption")),
    }
}

/// Encrypts a package (the zip bytes of an `.xlsx`, `.xlsm` or `.xlsb`)
/// with agile encryption: AES-256 in CBC mode, SHA-512, fresh random
/// salts and key, and an HMAC over the encrypted package, as Excel 2013
/// and later write it. Returns the bytes of the compound file to save.
pub fn encrypt_ooxml(
    package: &[u8],
    password: &Password,
    options: &EncryptOptions,
) -> Result<Vec<u8>, CryptoError> {
    let max_spin = Limits::default().max_spin_count;
    if options.spin_count == 0 {
        return Err(CryptoError::invalid(
            "a spin count of 0 (no key stretching); use at least 1, and 100,000 to match Excel",
        ));
    }
    if options.spin_count > max_spin {
        return Err(CryptoError::limit("spin count", u64::from(max_spin)));
    }
    let (info, encrypted) = agile::encrypt(package, password, options.spin_count)?;
    write_container(&info, &encrypted)
}

/// Writes the compound file: `EncryptionInfo`, `EncryptedPackage` and
/// the `\x06DataSpaces` storage (MS-OFFCRYPTO 2.1.1, 2.3.4.1).
fn write_container(info: &[u8], package: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let failed = |e: CfbError| CryptoError::WriteFailed {
        detail: e.to_string(),
    };
    let mut cf = Builder::new(Version::V3);
    let ds = "/\u{6}DataSpaces";
    cf.create_storage(ds).map_err(failed)?;
    cf.create_storage(&format!("{ds}/DataSpaceInfo"))
        .map_err(failed)?;
    cf.create_storage(&format!("{ds}/TransformInfo"))
        .map_err(failed)?;
    cf.create_storage(&format!("{ds}/TransformInfo/StrongEncryptionTransform"))
        .map_err(failed)?;
    let streams: [(String, Vec<u8>); 6] = [
        (format!("{ds}/Version"), dataspace_version()),
        (format!("{ds}/DataSpaceMap"), dataspace_map()),
        (
            format!("{ds}/DataSpaceInfo/StrongEncryptionDataSpace"),
            dataspace_definition(),
        ),
        (
            format!("{ds}/TransformInfo/StrongEncryptionTransform/\u{6}Primary"),
            transform_info(),
        ),
        ("/EncryptionInfo".into(), info.to_vec()),
        ("/EncryptedPackage".into(), package.to_vec()),
    ];
    for (path, bytes) in streams {
        cf.create_stream(&path, bytes).map_err(failed)?;
    }
    cf.to_bytes().map_err(failed)
}

/// UNICODE-LP-P4: byte length, UTF-16LE text, padding to 4 bytes.
fn lp_p4(out: &mut Vec<u8>, text: &str) {
    let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    out.extend_from_slice(&units);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn versions_1_0(out: &mut Vec<u8>) {
    // Reader, updater and writer versions: 1.0 each.
    for _ in 0..3 {
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
    }
}

/// `\x06DataSpaces/Version` (DataSpaceVersionInfo, 2.1.5).
fn dataspace_version() -> Vec<u8> {
    let mut out = Vec::new();
    lp_p4(&mut out, "Microsoft.Container.DataSpaces");
    versions_1_0(&mut out);
    out
}

/// `\x06DataSpaces/DataSpaceMap` (2.1.6): `EncryptedPackage` uses
/// `StrongEncryptionDataSpace`.
fn dataspace_map() -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&1u32.to_le_bytes()); // reference component count
    entry.extend_from_slice(&0u32.to_le_bytes()); // component type: stream
    lp_p4(&mut entry, "EncryptedPackage");
    lp_p4(&mut entry, "StrongEncryptionDataSpace");
    let mut out = Vec::new();
    out.extend_from_slice(&8u32.to_le_bytes()); // header length
    out.extend_from_slice(&1u32.to_le_bytes()); // entry count
    out.extend_from_slice(&(entry.len() as u32 + 4).to_le_bytes());
    out.extend_from_slice(&entry);
    out
}

/// `DataSpaceInfo/StrongEncryptionDataSpace` (DataSpaceDefinition, 2.1.7).
fn dataspace_definition() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&8u32.to_le_bytes()); // header length
    out.extend_from_slice(&1u32.to_le_bytes()); // transform count
    lp_p4(&mut out, "StrongEncryptionTransform");
    out
}

/// `TransformInfo/StrongEncryptionTransform/\x06Primary`
/// (TransformInfoHeader 2.1.8 + EncryptionTransformInfo 2.1.9).
fn transform_info() -> Vec<u8> {
    let mut id = Vec::new();
    lp_p4(&mut id, "{FF9A3F03-56EF-4613-BDD5-5A41C1D07246}");
    let mut out = Vec::new();
    // Transform length: the bytes before the transform name.
    out.extend_from_slice(&(8 + id.len() as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // transform type
    out.extend_from_slice(&id);
    lp_p4(&mut out, "Microsoft.Container.EncryptionTransform");
    versions_1_0(&mut out);
    out.extend_from_slice(&0u32.to_le_bytes()); // encryption name: empty
    out.extend_from_slice(&0u32.to_le_bytes()); // encryption block size
    out.extend_from_slice(&0u32.to_le_bytes()); // cipher mode
    out.extend_from_slice(&4u32.to_le_bytes()); // reserved
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn transform_header_length_is_0x58() {
        let t = super::transform_info();
        assert_eq!(&t[..4], &0x58u32.to_le_bytes());
    }
}
