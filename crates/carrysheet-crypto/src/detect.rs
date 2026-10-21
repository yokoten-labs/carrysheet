//! Find out how a file is protected, without a password.

use carrysheet_cfb::{CfbError, CompoundFile, Entry};

use crate::error::CryptoError;
use crate::util::container_error;
use crate::{Limits, biff, ooxml};

pub(crate) const OLE2_MAGIC: [u8; 8] = carrysheet_cfb::SIGNATURE;

/// How a file is protected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Protection {
    /// `.xlsx` family, agile encryption (Office 2010 and later).
    Agile(AgileSummary),
    /// `.xlsx` family, standard encryption (ECMA-376, Office 2007).
    Standard {
        /// AES key size in bits (128, 192 or 256).
        key_bits: u32,
    },
    /// `.xlsx` family, extensible encryption (a third-party encryption
    /// module). Not decryptable here.
    Extensible,
    /// Microsoft Rights Management: a Purview sensitivity label with
    /// encryption, or Information Rights Management. No password opens
    /// it; see [`CryptoError::RightsManagement`] for the explanation to
    /// show.
    RightsManagement,
    /// `.xls`, protected by a FILEPASS record.
    Biff(BiffEncryption),
}

impl Protection {
    /// The method in a few plain words, such as `agile encryption (AES-256, SHA512)`.
    pub fn describe(&self) -> String {
        match self {
            Protection::Agile(a) => {
                format!("agile encryption ({}-{}, {})", a.cipher, a.key_bits, a.hash)
            }
            Protection::Standard { key_bits } => {
                format!("standard encryption (AES-{key_bits}, SHA1)")
            }
            Protection::Extensible => "extensible encryption".into(),
            Protection::RightsManagement => "Rights Management (a sensitivity label or IRM)".into(),
            Protection::Biff(b) => b.describe(),
        }
    }

    /// Whether a password can open it (false for Rights Management and
    /// methods this crate cannot decrypt).
    pub fn password_can_open(&self) -> bool {
        match self {
            Protection::Agile(a) => a.password && a.cipher == "AES",
            Protection::Standard { .. } | Protection::Biff(_) => true,
            Protection::Extensible | Protection::RightsManagement => false,
        }
    }
}

/// The parameters of agile encryption, as declared in `EncryptionInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgileSummary {
    /// Cipher name, such as `AES`.
    pub cipher: String,
    /// Key size in bits.
    pub key_bits: u32,
    /// Hash name, such as `SHA512`.
    pub hash: String,
    /// Key-stretching rounds for the password (0 if there is no
    /// password key encryptor).
    pub spin_count: u32,
    /// Whether a password can open it.
    pub password: bool,
    /// Whether a certificate can open it (not supported here).
    pub certificate: bool,
}

/// How a `.xls` workbook stream is protected (MS-XLS FilePass record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BiffEncryption {
    /// XOR obfuscation (Excel 95 style, weak).
    Xor,
    /// RC4 with an MD5-based key (Excel 97-2000).
    Rc4,
    /// RC4 with a SHA-1 key through CryptoAPI (Excel 2002-2003, and what
    /// later Excel versions write).
    Rc4CryptoApi {
        /// Key size in bits (40 to 128).
        key_bits: u32,
    },
}

impl BiffEncryption {
    /// The method in a few plain words.
    pub fn describe(&self) -> String {
        match self {
            BiffEncryption::Xor => "XOR obfuscation".into(),
            BiffEncryption::Rc4 => "RC4 encryption".into(),
            BiffEncryption::Rc4CryptoApi { key_bits } => {
                format!("RC4 CryptoAPI encryption ({key_bits}-bit)")
            }
        }
    }
}

/// Finds out how a file is protected. No password is needed.
///
/// Takes the whole file. Returns `None` for files that are not
/// protected, including zip-based `.xlsx` files and anything that is not
/// a compound file. For `.xls`, the FILEPASS record at the start of the
/// `Workbook` stream is read; for a `Workbook` stream already in hand,
/// use [`detect_workbook_stream`](crate::detect_workbook_stream).
pub fn detect(file: &[u8], limits: &Limits) -> Result<Option<Protection>, CryptoError> {
    if !file.starts_with(&OLE2_MAGIC) {
        return Ok(None);
    }
    check_input(file, limits)?;
    let cf = open(file)?;
    if is_rights_managed(&cf, limits)? {
        return Ok(Some(Protection::RightsManagement));
    }
    if cf.is_stream("/EncryptionInfo") {
        let info = read_stream(
            &cf,
            "/EncryptionInfo",
            limits.max_info_bytes,
            "EncryptionInfo",
        )?;
        return ooxml::describe(&info, limits).map(Some);
    }
    if cf.is_stream("/Workbook") {
        // FILEPASS sits right after the first BOF; 64 KB is plenty.
        let head = read_prefix(&cf, "/Workbook", 64 * 1024)?;
        return Ok(biff::detect_prefix(&head)?.map(Protection::Biff));
    }
    Ok(None)
}

pub(crate) fn check_input(file: &[u8], limits: &Limits) -> Result<(), CryptoError> {
    if file.len() > limits.max_input_bytes {
        return Err(CryptoError::limit(
            "input bytes",
            limits.max_input_bytes as u64,
        ));
    }
    Ok(())
}

pub(crate) fn open(file: &[u8]) -> Result<CompoundFile<'_>, CryptoError> {
    if !file.starts_with(&OLE2_MAGIC) {
        return Err(CryptoError::NotCompoundFile);
    }
    CompoundFile::open(file).map_err(container_error)
}

fn stream_entry<'f>(cf: &'f CompoundFile<'_>, path: &str) -> Result<&'f Entry, CryptoError> {
    cf.entry(path).filter(|e| e.is_stream()).ok_or_else(|| {
        container_error(CfbError::NotFound {
            path: path.to_string(),
        })
    })
}

/// Reads a whole stream, refusing one that says it is larger than `max`.
/// The buffer grows with the bytes really read, not the declared size.
pub(crate) fn read_stream(
    cf: &CompoundFile<'_>,
    path: &str,
    max: usize,
    part: &'static str,
) -> Result<Vec<u8>, CryptoError> {
    read_stream_with(cf, path, max, part, false)
}

/// Like [`read_stream`], but `short_ok` keeps the bytes that could be
/// read when the stream's sector chain ends before its declared size
/// (some writers, such as ExtenXLS, store a wrong size). The caller then
/// checks the data against its own size field.
pub(crate) fn read_stream_with(
    cf: &CompoundFile<'_>,
    path: &str,
    max: usize,
    part: &'static str,
    short_ok: bool,
) -> Result<Vec<u8>, CryptoError> {
    let entry = stream_entry(cf, path)?;
    let len = entry.size();
    if len > max as u64 {
        return Err(CryptoError::limit("stream bytes", max as u64));
    }
    // The reader allocates no more than the sector chain really holds.
    if short_ok {
        let out = cf.read_lenient(entry).map_err(container_error)?;
        if !out.is_empty() {
            return Ok(out);
        }
    }
    let out = cf.read(entry).map_err(container_error)?;
    if (out.len() as u64) < len {
        return Err(CryptoError::malformed(
            part,
            format!(
                "the stream says it holds {len} bytes but only {} could be read",
                out.len()
            ),
        ));
    }
    Ok(out)
}

fn read_prefix(cf: &CompoundFile<'_>, path: &str, max: u64) -> Result<Vec<u8>, CryptoError> {
    let entry = stream_entry(cf, path)?;
    cf.read_prefix(entry, max).map_err(container_error)
}

/// Rights Management leaves its marks in the `\x06DataSpaces` storage:
/// a data space or transform named for DRM (MS-OFFCRYPTO 2.2: IRMDS),
/// and the protected content in `\x09DRMContent`.
pub(crate) fn is_rights_managed(
    cf: &CompoundFile<'_>,
    limits: &Limits,
) -> Result<bool, CryptoError> {
    const MARKS: [&str; 5] = [
        "/\u{9}DRMContent",
        "/\u{6}DataSpaces/DataSpaceInfo/DRMEncryptedDataSpace",
        "/\u{6}DataSpaces/TransformInfo/DRMEncryptedTransform",
        "/\u{6}DataSpaces/DataSpaceInfo/\u{9}DRMDataSpace",
        "/\u{6}DataSpaces/TransformInfo/\u{9}DRMTransform",
    ];
    if MARKS.iter().any(|p| cf.exists(p)) {
        return Ok(true);
    }
    let map = "/\u{6}DataSpaces/DataSpaceMap";
    if cf.is_stream(map) {
        let bytes = read_stream(cf, map, limits.max_info_bytes, "DataSpaceMap")?;
        let needle: Vec<u8> = "DRM".encode_utf16().flat_map(u16::to_le_bytes).collect();
        if bytes.windows(needle.len()).any(|w| w == needle.as_slice()) {
            return Ok(true);
        }
    }
    Ok(false)
}
