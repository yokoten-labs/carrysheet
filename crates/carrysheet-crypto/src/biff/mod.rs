//! Encrypted `.xls` (BIFF8) workbook streams (MS-XLS 2.2.10, 2.4.117
//! FilePass; MS-OFFCRYPTO 2.3.5 - 2.3.7).
//!
//! Record headers (type and size) are never encrypted. Record bodies
//! after FILEPASS are, except BOF, FILEPASS, USREXCL, FILELOCK,
//! INTERFACEHDR, RRDINFO and RRDHEAD, and the first 4 bytes of BOUNDSHEET
//! (`lbPlyPos`, the stream position of the sheet). RC4 runs by stream
//! position, rekeyed every 1024 bytes; XOR uses position plus record size.
//!
//! Besides the `Workbook` stream, an encrypted `.xls` encrypts (MS-XLS
//! 2.2.10):
//!
//! - the pivot cache streams and the revision log stream, record by
//!   record like the workbook, from position 0 of each stream
//!   ([`DecryptedBiff::decrypt_record_stream`],
//!   [`BiffEncryptor::record_stream`]);
//! - with RC4 CryptoAPI, the document summary streams (unless the
//!   FILEPASS `fDocProps` flag is set) and a few other streams, which move
//!   into one stream named [`ENCRYPTION_STREAM`] (MS-OFFCRYPTO 2.3.5.4).

mod rc4;
mod xor;

use std::fmt;

use zeroize::Zeroizing;

use crate::detect::BiffEncryption;
use crate::error::CryptoError;
use crate::ooxml::{CryptoApiHeader, F_CRYPTOAPI, F_DOCPROPS, Verifier};
use crate::util::{Reader, random};
use crate::{Limits, Password, SaveKey, try_passwords};

use self::rc4::{KeyBase, Keystream};

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const FILEPASS: u16 = 0x002F;
const WRITEPROT: u16 = 0x0086;
const USREXCL: u16 = 0x0194;
const FILELOCK: u16 = 0x0195;
const INTERFACEHDR: u16 = 0x00E1;
const RRDINFO: u16 = 0x0196;
const RRDHEAD: u16 = 0x0138;
const BOUNDSHEET: u16 = 0x0085;
const INDEX: u16 = 0x020B;
const EXTSST: u16 = 0x00FF;

/// The BOF version of BIFF8 (Excel 97 and later).
const BIFF8: u16 = 0x0600;

/// The largest record body BIFF8 allows.
const MAX_RECORD: usize = 8224;
/// How many records to look at for FILEPASS before giving up.
const FILEPASS_SEARCH: usize = 64;

/// What the CryptoAPI header names as its provider when we write it.
const CSP_NAME: &str = "Microsoft Enhanced Cryptographic Provider v1.0";

/// The name of the stream that holds the encrypted document properties
/// (and a few other streams) of an RC4 CryptoAPI `.xls`.
pub const ENCRYPTION_STREAM: &str = "encryption";

const SUMMARY: &str = "\u{5}SummaryInformation";
const DOC_SUMMARY: &str = "\u{5}DocumentSummaryInformation";

/// The most entries an `encryption` stream may list. Excel writes two to
/// a handful; the cap keeps a file from making many tiny entries.
const MAX_PROTECTED_STREAMS: usize = 256;
/// Bytes of one stream descriptor without its name.
const DESCRIPTOR_FIXED: usize = 18;

/// A stream (or storage) kept inside the [`ENCRYPTION_STREAM`] stream.
#[derive(Clone, PartialEq, Eq)]
pub struct ProtectedStream {
    /// Its name in the compound file's root, such as
    /// `"\u{5}SummaryInformation"`.
    pub name: String,
    /// `true` for a storage: `data` is then a compound file holding the
    /// storage's contents (MS-OFFCRYPTO 2.3.5.4).
    pub storage: bool,
    /// The plain bytes.
    pub data: Vec<u8>,
}

impl ProtectedStream {
    /// A stream entry.
    pub fn stream(name: impl Into<String>, data: Vec<u8>) -> ProtectedStream {
        ProtectedStream {
            name: name.into(),
            storage: false,
            data,
        }
    }

    fn is_summary(&self) -> bool {
        !self.storage && (self.name == SUMMARY || self.name == DOC_SUMMARY)
    }
}

impl fmt::Debug for ProtectedStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtectedStream")
            .field("name", &self.name.escape_debug().to_string())
            .field("storage", &self.storage)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// The key that decrypts the other streams of an opened `.xls`.
enum StreamKey {
    Xor(Zeroizing<[u8; 16]>),
    Rc4(KeyBase),
}

/// A decrypted `.xls` workbook stream.
///
/// To open a whole `.xls`, the caller reads the compound file and:
///
/// 1. passes the `Workbook` stream, and the [`ENCRYPTION_STREAM`] stream if
///    there is one, to [`decrypt_biff_streams`];
/// 2. uses [`DecryptedBiff::protected_streams`] in place of the streams of
///    the same name (the `\u{5}DocumentSummaryInformation` in the file is
///    then an empty placeholder);
/// 3. decrypts every pivot cache stream (in `_SX_DB_CUR`) and the revision
///    log stream with [`DecryptedBiff::decrypt_record_stream`].
pub struct DecryptedBiff {
    /// The `Workbook` stream as if it had never been encrypted: the
    /// FILEPASS record is gone, and the stream positions that pointed past
    /// it (BOUNDSHEET `lbPlyPos`, INDEX, EXTSST) are moved back to match.
    /// Any BIFF8 reader can read it.
    pub stream: Vec<u8>,
    /// How it was protected.
    pub encryption: BiffEncryption,
    /// Whether the FILEPASS header says the document summary streams are
    /// encrypted too (CryptoAPI with `fDocProps` clear; MS-OFFCRYPTO
    /// 2.3.5.4). A save through [`SaveKey::biff_encryptor`] keeps them
    /// encrypted.
    pub doc_props_encrypted: bool,
    /// The decrypted contents of the [`ENCRYPTION_STREAM`] stream, in the
    /// file's order: the real summary streams and the other streams kept
    /// there. Empty when no such stream was passed.
    pub protected_streams: Vec<ProtectedStream>,
    /// What a save needs to keep the file protected.
    pub key: SaveKey,
    stream_key: StreamKey,
}

impl fmt::Debug for DecryptedBiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecryptedBiff")
            .field("stream", &format_args!("<{} bytes>", self.stream.len()))
            .field("encryption", &self.encryption)
            .field("doc_props_encrypted", &self.doc_props_encrypted)
            .field("protected_streams", &self.protected_streams)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl DecryptedBiff {
    /// Decrypts a stream of BIFF records encrypted along with the
    /// workbook: a pivot cache stream or the revision log stream (MS-XLS
    /// 2.2.10). The same records stay plain as in the workbook, and the
    /// key position starts at 0 for each stream. The result has the same
    /// length as the input.
    pub fn decrypt_record_stream(&self, stream: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = stream.to_vec();
        match &self.stream_key {
            StreamKey::Xor(array) => xor_records(&mut out, 0, array)?,
            StreamKey::Rc4(base) => apply_rc4(&mut out, 0, base)?,
        }
        Ok(out)
    }
}

/// A record header found in the stream.
#[derive(Debug, Clone, Copy)]
struct Rec {
    /// Stream position of the record header.
    off: usize,
    ty: u16,
    len: usize,
}

impl Rec {
    fn body(&self) -> usize {
        self.off + 4
    }
    fn end(&self) -> usize {
        self.off + 4 + self.len
    }
}

/// Walks record headers. With `strict`, a record that runs past the end
/// is an error; without, the walk just stops (for a stream prefix).
struct Records<'a> {
    stream: &'a [u8],
    pos: usize,
    strict: bool,
    failed: bool,
}

impl<'a> Records<'a> {
    fn new(stream: &'a [u8], strict: bool) -> Self {
        Records {
            stream,
            pos: 0,
            strict,
            failed: false,
        }
    }
}

impl Iterator for Records<'_> {
    type Item = Result<Rec, CryptoError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match record_at(self.stream, self.pos) {
            Ok(Some(rec)) => {
                self.pos = rec.end();
                Some(Ok(rec))
            }
            Ok(None) => None,
            Err(e) => {
                self.failed = true;
                if self.strict { Some(Err(e)) } else { None }
            }
        }
    }
}

/// The record header at `pos`, or `None` when fewer than 4 bytes are
/// left. A record body that runs past the end is an error.
fn record_at(stream: &[u8], pos: usize) -> Result<Option<Rec>, CryptoError> {
    let Some(head) = pos.checked_add(4).and_then(|end| stream.get(pos..end)) else {
        return Ok(None);
    };
    let ty = u16::from_le_bytes([head[0], head[1]]);
    let len = usize::from(u16::from_le_bytes([head[2], head[3]]));
    let rec = Rec { off: pos, ty, len };
    if rec.end() > stream.len() {
        return Err(CryptoError::malformed(
            "Workbook stream",
            format!(
                "record 0x{ty:04X} at byte {pos} says it has {len} bytes, but only {} are left",
                stream.len() - rec.body().min(stream.len())
            ),
        ));
    }
    Ok(Some(rec))
}

/// The parsed FILEPASS record body.
enum FilePass {
    Xor {
        key: u16,
        verifier: u16,
    },
    Rc4 {
        salt: Vec<u8>,
        verifier: Vec<u8>,
        verifier_hash: Vec<u8>,
    },
    CryptoApi {
        key_bits: u32,
        doc_props: bool,
        verifier: Verifier,
    },
}

impl FilePass {
    fn parse(data: &[u8]) -> Result<FilePass, CryptoError> {
        let mut r = Reader::new(data, "FILEPASS record");
        match r.u16("the encryption type")? {
            0 => Ok(FilePass::Xor {
                key: r.u16("the XOR key")?,
                verifier: r.u16("the XOR verifier")?,
            }),
            1 => {
                let major = r.u16("the major version")?;
                let minor = r.u16("the minor version")?;
                match (major, minor) {
                    (1, 1) => Ok(FilePass::Rc4 {
                        salt: r.take(16, "the salt")?.to_vec(),
                        verifier: r.take(16, "the encrypted verifier")?.to_vec(),
                        verifier_hash: r.take(16, "the encrypted verifier hash")?.to_vec(),
                    }),
                    (2..=4, 2) => {
                        let header = CryptoApiHeader::parse(&mut r)?;
                        let verifier = Verifier::parse(&mut r, 20)?;
                        if !matches!(header.alg_id, 0 | 0x6801) {
                            return Err(CryptoError::unsupported(format!(
                                "CryptoAPI encryption with algorithm 0x{:04X} (only RC4 is used in .xls)",
                                header.alg_id
                            )));
                        }
                        if !matches!(header.alg_id_hash, 0 | 0x8004) {
                            return Err(CryptoError::unsupported(format!(
                                "CryptoAPI encryption with hash algorithm 0x{:04X}",
                                header.alg_id_hash
                            )));
                        }
                        let key_bits = if header.key_bits == 0 {
                            40
                        } else {
                            header.key_bits
                        };
                        if !(40..=128).contains(&key_bits) || key_bits % 8 != 0 {
                            return Err(CryptoError::malformed(
                                "FILEPASS record",
                                format!(
                                    "the RC4 key size is {key_bits} bits; it must be 40 to 128, in steps of 8"
                                ),
                            ));
                        }
                        Ok(FilePass::CryptoApi {
                            key_bits,
                            // fDocProps set means the properties are NOT encrypted.
                            doc_props: header.flags & F_DOCPROPS == 0,
                            verifier,
                        })
                    }
                    _ => Err(CryptoError::unsupported(format!(
                        "RC4 encryption version {major}.{minor}"
                    ))),
                }
            }
            other => Err(CryptoError::unsupported(format!(
                "FILEPASS encryption type {other}"
            ))),
        }
    }

    fn kind(&self) -> BiffEncryption {
        match self {
            FilePass::Xor { .. } => BiffEncryption::Xor,
            FilePass::Rc4 { .. } => BiffEncryption::Rc4,
            FilePass::CryptoApi { key_bits, .. } => BiffEncryption::Rc4CryptoApi {
                key_bits: *key_bits,
            },
        }
    }
}

/// Reads the body of a FILEPASS record (type 0x002F) and says how the
/// workbook is protected. For a BIFF8 reader that meets FILEPASS itself.
pub fn detect_filepass(record_body: &[u8]) -> Result<BiffEncryption, CryptoError> {
    FilePass::parse(record_body).map(|f| f.kind())
}

/// Says whether a BIFF8 `Workbook` stream is protected, and how.
///
/// Reads the first records of the workbook globals; FILEPASS comes right
/// after BOF (and WRITEPROT, if the workbook is write-protected).
pub fn detect_workbook_stream(stream: &[u8]) -> Result<Option<BiffEncryption>, CryptoError> {
    Ok(find_filepass(stream, true)?.map(|(_, f)| f.kind()))
}

pub(crate) fn detect_prefix(stream: &[u8]) -> Result<Option<BiffEncryption>, CryptoError> {
    Ok(find_filepass(stream, false)?.map(|(_, f)| f.kind()))
}

fn find_filepass(stream: &[u8], strict: bool) -> Result<Option<(Rec, FilePass)>, CryptoError> {
    let mut records = Records::new(stream, strict);
    let biff8 = match records.next() {
        Some(Ok(r)) if r.ty == BOF => is_biff8(stream, &r),
        Some(Err(e)) => return Err(e),
        None if !strict => return Ok(None),
        _ => {
            return Err(CryptoError::malformed(
                "Workbook stream",
                "it does not start with a BOF record, so it is not BIFF8",
            ));
        }
    };
    for rec in records.take(FILEPASS_SEARCH) {
        let rec = rec?;
        match rec.ty {
            FILEPASS if !biff8 => {
                return Err(CryptoError::unsupported(
                    "Excel 5.0/95 (BIFF5) workbook encryption",
                ));
            }
            FILEPASS => {
                let body = &stream[rec.body()..rec.end()];
                return Ok(Some((rec, FilePass::parse(body)?)));
            }
            EOF | BOF => break,
            _ => {}
        }
    }
    Ok(None)
}

/// Whether a BOF record says BIFF8 (its first field, `vers`, is 0x0600).
fn is_biff8(stream: &[u8], bof: &Rec) -> bool {
    bof.len >= 2 && stream[bof.body()..bof.body() + 2] == BIFF8.to_le_bytes()
}

fn never_encrypted(ty: u16) -> bool {
    matches!(
        ty,
        BOF | FILEPASS | USREXCL | FILELOCK | INTERFACEHDR | RRDINFO | RRDHEAD
    )
}

/// The part of a record body that is encrypted, as a stream range.
fn encrypted_range(rec: &Rec) -> Option<(usize, usize)> {
    if never_encrypted(rec.ty) {
        return None;
    }
    let skip = if rec.ty == BOUNDSHEET {
        rec.len.min(4)
    } else {
        0
    };
    Some((rec.body() + skip, rec.end()))
}

/// Decrypts a password-protected BIFF8 `Workbook` stream.
///
/// Tries `password` first, then Excel's default password
/// ([`DEFAULT_PASSWORD`](crate::DEFAULT_PASSWORD)), so write-protected
/// workbooks open with `None`. The result is the same length as the
/// input minus the FILEPASS record; nothing else is allocated from sizes
/// in the file. Same as [`decrypt_biff_streams`] without an
/// `encryption` stream.
///
/// A workbook whose document properties are encrypted (RC4 CryptoAPI with
/// `fDocProps` clear) is refused with [`CryptoError::InvalidArgument`]:
/// its title and author are in the `encryption` stream, and a save
/// without them would lose them. Use [`decrypt_biff_streams`] for it.
pub fn decrypt_biff(
    workbook_stream: &[u8],
    password: Option<&Password>,
    limits: &Limits,
) -> Result<DecryptedBiff, CryptoError> {
    if let Some((
        _,
        FilePass::CryptoApi {
            doc_props: true, ..
        },
    )) = find_filepass(workbook_stream, true)?
    {
        return Err(CryptoError::invalid(format!(
            "this workbook's document properties are encrypted in its `{ENCRYPTION_STREAM}` stream; open it with decrypt_biff_streams and pass that stream, so a save keeps them"
        )));
    }
    decrypt_biff_streams(workbook_stream, None, password, limits)
}

/// Decrypts a BIFF8 `Workbook` stream and, when the file has one, its
/// [`ENCRYPTION_STREAM`] stream (the encrypted document properties of an
/// RC4 CryptoAPI file), into [`DecryptedBiff::protected_streams`].
///
/// The decrypted entries together are never longer than the
/// `encryption` stream, and there are at most 256 of them.
pub fn decrypt_biff_streams(
    workbook_stream: &[u8],
    encryption_stream: Option<&[u8]>,
    password: Option<&Password>,
    limits: &Limits,
) -> Result<DecryptedBiff, CryptoError> {
    let stream = workbook_stream;
    let input = stream.len() + encryption_stream.map_or(0, <[u8]>::len);
    if input > limits.max_input_bytes {
        return Err(CryptoError::limit(
            "input bytes",
            limits.max_input_bytes as u64,
        ));
    }
    let Some((fp, filepass)) = find_filepass(stream, true)? else {
        return Err(CryptoError::NotEncrypted);
    };
    if encryption_stream.is_some() && !matches!(filepass, FilePass::CryptoApi { .. }) {
        return Err(CryptoError::malformed(
            "encryption stream",
            "the file has one, but its workbook is not encrypted with RC4 CryptoAPI, the only method that uses it",
        ));
    }
    let encryption = filepass.kind();
    let mut out = stream.to_vec();
    let mut doc_props_encrypted = false;
    let from = fp.end();
    let (stream_key, key) = match &filepass {
        FilePass::Xor { key, verifier } => {
            let (array, key) = try_passwords(password, true, |p| xor::open(p, *key, *verifier))?;
            xor_records(&mut out, from, &array)?;
            (StreamKey::Xor(array), key)
        }
        FilePass::Rc4 {
            salt,
            verifier,
            verifier_hash,
        } => {
            let (base, key) = try_passwords(password, true, |p| {
                let base = KeyBase::md5(&p.utf16le()?, salt);
                base.verify(verifier, verifier_hash)?;
                Ok(base)
            })?;
            apply_rc4(&mut out, from, &base)?;
            (StreamKey::Rc4(base), key)
        }
        FilePass::CryptoApi {
            key_bits,
            doc_props,
            verifier,
        } => {
            doc_props_encrypted = *doc_props;
            let (base, key) = try_passwords(password, true, |p| {
                let base = KeyBase::cryptoapi(&p.utf16le()?, &verifier.salt, *key_bits);
                base.verify(
                    &verifier.encrypted_verifier,
                    &verifier.encrypted_verifier_hash,
                )?;
                Ok(base)
            })?;
            apply_rc4(&mut out, from, &base)?;
            (
                StreamKey::Rc4(base),
                key.with_doc_props_encrypted(*doc_props),
            )
        }
    };
    let protected_streams = match (encryption_stream, &stream_key) {
        (Some(bytes), StreamKey::Rc4(base)) => read_encryption_stream(bytes, base)?,
        _ => Vec::new(),
    };
    // Take FILEPASS out and move every stream position behind it back.
    out.drain(fp.off..fp.end());
    let removed = (fp.end() - fp.off) as i64;
    shift_positions(&mut out, fp.end(), -removed)?;
    Ok(DecryptedBiff {
        stream: out,
        encryption,
        doc_props_encrypted,
        protected_streams,
        key,
        stream_key,
    })
}

/// XOR-decrypts every record body from `from` on.
fn xor_records(buf: &mut [u8], from: usize, array: &[u8; 16]) -> Result<(), CryptoError> {
    crypt_records(buf, from, |buf, rec, start, end| {
        for (p, b) in buf[start..end].iter_mut().enumerate() {
            *b = xor::decrypt_byte(*b, array, start + p, rec.len);
        }
        Ok(())
    })
}

/// `bytes` with a fresh RC4 keystream for `block`, from its first byte.
fn rc4_block(base: &KeyBase, block: u32, bytes: &mut [u8]) -> Result<(), CryptoError> {
    use ::rc4::StreamCipher;
    base.cipher(block)?.apply_keystream(bytes);
    Ok(())
}

fn bad_encryption_stream(detail: impl Into<String>) -> CryptoError {
    CryptoError::malformed("encryption stream", detail)
}

/// Reads the `encryption` stream (MS-OFFCRYPTO 2.3.5.4): an 8-byte header
/// (where the descriptor array is, and its size), the entries' bytes, and
/// the descriptor array. The header and the array each use a fresh block 0
/// keystream; each entry uses a fresh keystream for its own block.
fn read_encryption_stream(
    bytes: &[u8],
    base: &KeyBase,
) -> Result<Vec<ProtectedStream>, CryptoError> {
    let mut head = Zeroizing::new([0u8; 8]);
    head.copy_from_slice(
        bytes
            .get(..8)
            .ok_or_else(|| bad_encryption_stream("it is shorter than its 8-byte header"))?,
    );
    rc4_block(base, 0, &mut head[..])?;
    let offset = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let array = offset
        .checked_add(size)
        .filter(|&end| offset >= 8 && size >= 4 && end <= bytes.len())
        .map(|end| &bytes[offset..end])
        .ok_or_else(|| {
            bad_encryption_stream(format!(
                "its descriptor array (at byte {offset}, {size} bytes) is not inside the {} bytes of the stream",
                bytes.len()
            ))
        })?;
    let mut array = array.to_vec();
    rc4_block(base, 0, &mut array)?;
    let mut r = Reader::new(&array, "encryption stream");
    let count = r.u32("the descriptor count")? as usize;
    if count > MAX_PROTECTED_STREAMS || count > r.rest().len() / DESCRIPTOR_FIXED {
        return Err(bad_encryption_stream(format!(
            "it lists {count} streams; at most {} fit its descriptor array, and at most {MAX_PROTECTED_STREAMS} are read",
            (r.rest().len() / DESCRIPTOR_FIXED).min(MAX_PROTECTED_STREAMS)
        )));
    }
    // Every entry's bytes come from the stream, so all of them together
    // may not be longer than it: overlapping entries cannot multiply it.
    let mut budget = bytes.len();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = r.u32("a stream offset")? as usize;
        let len = r.u32("a stream size")? as usize;
        let block = r.u16("a block number")?;
        let name_units = usize::from(r.take(1, "a name size")?[0]);
        let flags = r.take(1, "the descriptor flags")?[0];
        r.u32("a reserved field")?;
        let name_bytes = r.take(name_units * 2, "a stream name")?;
        r.u16("a name terminator")?;
        let units: Vec<u16> = name_bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        let name = String::from_utf16(&units)
            .ok()
            .filter(|n| !n.is_empty() && !n.contains('\0'))
            .ok_or_else(|| {
                bad_encryption_stream(format!("entry {i} has an empty or invalid name"))
            })?;
        let data = start
            .checked_add(len)
            .filter(|&end| start >= 8 && end <= offset)
            .map(|end| &bytes[start..end])
            .ok_or_else(|| {
                bad_encryption_stream(format!(
                    "entry {i} (byte {start}, {len} bytes) is not inside the data part of the stream"
                ))
            })?;
        budget = budget.checked_sub(len).ok_or_else(|| {
            bad_encryption_stream(
                "its entries together are longer than the stream, so some overlap",
            )
        })?;
        let mut data = data.to_vec();
        rc4_block(base, u32::from(block), &mut data)?;
        out.push(ProtectedStream {
            name,
            storage: flags & 1 == 0,
            data,
        });
    }
    Ok(out)
}

/// Calls `f(buf, record, start, end)` for the encrypted range of every
/// record that starts at or after `from`. Record headers are never
/// encrypted, so the walk reads them from `buf` while `f` changes bodies.
fn crypt_records(
    buf: &mut [u8],
    from: usize,
    mut f: impl FnMut(&mut [u8], &Rec, usize, usize) -> Result<(), CryptoError>,
) -> Result<(), CryptoError> {
    let mut pos = 0;
    while let Some(rec) = record_at(buf, pos)? {
        pos = rec.end();
        if rec.off < from {
            continue;
        }
        if let Some((start, end)) = encrypted_range(&rec) {
            f(buf, &rec, start, end)?;
        }
    }
    Ok(())
}

/// RC4 is its own inverse: this encrypts and decrypts.
fn apply_rc4(buf: &mut [u8], from: usize, base: &KeyBase) -> Result<(), CryptoError> {
    let mut ks = Keystream::new(base);
    crypt_records(buf, from, |buf, _, start, end| {
        ks.apply(start, &mut buf[start..end])
    })
}

/// Adds `delta` to every absolute stream position at or after
/// `threshold`: BOUNDSHEET `lbPlyPos`, INDEX `ibXF` and `rgibRw`, and
/// EXTSST `ib`. Works on a plain stream, in place, without allocating.
fn shift_positions(stream: &mut [u8], threshold: usize, delta: i64) -> Result<(), CryptoError> {
    let mut pos = 0;
    while let Some(rec) = record_at(stream, pos)? {
        pos = rec.end();
        // (first field, distance between fields)
        let (first, step) = match rec.ty {
            BOUNDSHEET if rec.len >= 4 => (rec.body(), rec.len),
            INDEX if rec.len >= 16 => {
                shift_one(stream, rec.body() + 12, threshold, delta, &rec)?;
                (rec.body() + 16, 4)
            }
            EXTSST if rec.len >= 2 => (rec.body() + 2, 8),
            _ => continue,
        };
        let mut at = first;
        while at + step.max(4) <= rec.end() {
            shift_one(stream, at, threshold, delta, &rec)?;
            at += step;
        }
    }
    Ok(())
}

fn shift_one(
    stream: &mut [u8],
    at: usize,
    threshold: usize,
    delta: i64,
    rec: &Rec,
) -> Result<(), CryptoError> {
    let mut b = [0u8; 4];
    b.copy_from_slice(&stream[at..at + 4]);
    let v = u32::from_le_bytes(b);
    if (v as usize) < threshold {
        return Ok(());
    }
    let moved = u32::try_from(i64::from(v) + delta).map_err(|_| {
        CryptoError::malformed(
            "Workbook stream",
            format!(
                "record 0x{:04X} at byte {} holds a stream position that cannot be moved",
                rec.ty, rec.off
            ),
        )
    })?;
    stream[at..at + 4].copy_from_slice(&moved.to_le_bytes());
    Ok(())
}

/// Encrypts a BIFF8 `Workbook` stream with RC4 CryptoAPI (128-bit), as
/// Excel 2003 and later write it, with plain document properties.
///
/// Same as `BiffEncryptor::new(password, false)?.workbook(stream)`; use a
/// [`BiffEncryptor`] (from [`SaveKey::biff_encryptor`] on a save) to
/// encrypt the other streams of the same file with the same key.
pub fn encrypt_biff(workbook_stream: &[u8], password: &Password) -> Result<Vec<u8>, CryptoError> {
    BiffEncryptor::new(password, false)?.workbook(workbook_stream)
}

/// Writes the encrypted streams of one `.xls` with RC4 CryptoAPI 128-bit,
/// with one fresh random salt for all of them.
///
/// To save a whole `.xls`, the caller writes a compound file with:
///
/// - `Workbook` from [`BiffEncryptor::workbook`];
/// - every pivot cache stream and the revision log stream from
///   [`BiffEncryptor::record_stream`];
/// - when [`BiffEncryptor::doc_props_encrypted`]: the [`ENCRYPTION_STREAM`]
///   stream from [`BiffEncryptor::encryption_stream`] with the real
///   summary streams in it, `\u{5}DocumentSummaryInformation` set to
///   [`empty_document_summary`], no `\u{5}SummaryInformation`, and no
///   plain copy of anything that went into the `encryption` stream.
///   Otherwise the summary streams are written plain, as usual.
pub struct BiffEncryptor {
    base: KeyBase,
    salt: Zeroizing<Vec<u8>>,
    verifier: Zeroizing<Vec<u8>>,
    doc_props_encrypted: bool,
}

impl fmt::Debug for BiffEncryptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BiffEncryptor")
            .field("doc_props_encrypted", &self.doc_props_encrypted)
            .finish_non_exhaustive()
    }
}

impl BiffEncryptor {
    /// A new key from `password`, with a fresh random salt. With
    /// `doc_props_encrypted`, the FILEPASS record says the document
    /// summary streams are encrypted (`fDocProps` clear), and they must
    /// go into [`BiffEncryptor::encryption_stream`].
    pub fn new(
        password: &Password,
        doc_props_encrypted: bool,
    ) -> Result<BiffEncryptor, CryptoError> {
        let pw = password.utf16le()?;
        let salt = random(16)?;
        let verifier = random(16)?;
        let base = KeyBase::cryptoapi(&pw, &salt, 128);
        Ok(BiffEncryptor {
            base,
            salt,
            verifier,
            doc_props_encrypted,
        })
    }

    /// Whether the summary streams go into the `encryption` stream.
    pub fn doc_props_encrypted(&self) -> bool {
        self.doc_props_encrypted
    }

    /// Encrypts a plain BIFF8 `Workbook` stream (no FILEPASS), such as
    /// [`DecryptedBiff::stream`] or a freshly written workbook.
    ///
    /// A FILEPASS record goes in right after the first BOF (and WRITEPROT,
    /// if present), and the stream positions behind it (BOUNDSHEET, INDEX,
    /// EXTSST) move to match, so decrypting gives back the same stream.
    /// Excel 5.0/95 (BIFF5) streams are refused.
    pub fn workbook(&self, workbook_stream: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let stream = workbook_stream;
        if find_filepass(stream, true)?.is_some() {
            return Err(CryptoError::malformed(
                "Workbook stream",
                "it already has a FILEPASS record; pass the decrypted stream",
            ));
        }
        // Where FILEPASS goes: after BOF, and after WRITEPROT if it follows.
        let mut records = Records::new(stream, true);
        let bof = match records.next() {
            Some(Ok(r)) => r,
            Some(Err(e)) => return Err(e),
            None => {
                return Err(CryptoError::malformed("Workbook stream", "it is empty"));
            }
        };
        if !is_biff8(stream, &bof) {
            return Err(CryptoError::invalid(
                "the workbook is not BIFF8 (Excel 97 and later), perhaps BIFF5 (Excel 5.0/95), and only BIFF8 workbooks can be encrypted; save it as a BIFF8 .xls or as .xlsx first",
            ));
        }
        let mut at = bof.end();
        if let Some(Ok(r)) = records.next()
            && r.ty == WRITEPROT
        {
            at = r.end();
        }

        let body = cryptoapi_filepass(
            &self.base,
            &self.salt,
            &self.verifier,
            self.doc_props_encrypted,
        )?;
        if body.len() > MAX_RECORD {
            return Err(CryptoError::malformed("FILEPASS record", "it is too long"));
        }

        let mut out = Vec::with_capacity(stream.len() + 4 + body.len());
        out.extend_from_slice(&stream[..at]);
        out.extend_from_slice(&FILEPASS.to_le_bytes());
        out.extend_from_slice(&(body.len() as u16).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&stream[at..]);
        shift_positions(&mut out, at, (4 + body.len()) as i64)?;

        apply_rc4(&mut out, at + 4 + body.len(), &self.base)?;
        Ok(out)
    }

    /// Encrypts a pivot cache stream or the revision log stream: every
    /// record body (except the ones that always stay plain), with the key
    /// position starting at 0 for this stream (MS-XLS 2.2.10).
    pub fn record_stream(&self, stream: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = stream.to_vec();
        apply_rc4(&mut out, 0, &self.base)?;
        Ok(out)
    }

    /// Writes the [`ENCRYPTION_STREAM`] stream (MS-OFFCRYPTO 2.3.5.4) that
    /// holds `entries`, such as [`DecryptedBiff::protected_streams`]. The
    /// summary streams are written last, as the spec orders; each entry
    /// gets its own block number.
    ///
    /// Summary streams are refused unless the encryptor was made with
    /// `doc_props_encrypted`, because the FILEPASS record would tell
    /// readers to look for them in the plain streams. With
    /// `doc_props_encrypted`, an empty list is refused: the document
    /// properties would be lost.
    pub fn encryption_stream(&self, entries: &[ProtectedStream]) -> Result<Vec<u8>, CryptoError> {
        if self.doc_props_encrypted && entries.is_empty() {
            return Err(CryptoError::invalid(
                "the document properties are encrypted, so the encryption stream needs the summary streams; pass DecryptedBiff::protected_streams (from decrypt_biff_streams), or make the encryptor without doc_props_encrypted",
            ));
        }
        if entries.len() > MAX_PROTECTED_STREAMS {
            return Err(CryptoError::invalid(format!(
                "the encryption stream can hold at most {MAX_PROTECTED_STREAMS} entries, not {}",
                entries.len()
            )));
        }
        if !self.doc_props_encrypted && entries.iter().any(ProtectedStream::is_summary) {
            return Err(CryptoError::invalid(
                "the summary streams can go into the encryption stream only when the document properties are encrypted; make the encryptor with doc_props_encrypted, or write them as plain streams",
            ));
        }
        // Other entries first, then SummaryInformation, then
        // DocumentSummaryInformation.
        let rank = |e: &ProtectedStream| match (e.is_summary(), e.name == DOC_SUMMARY) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        };
        let mut ordered: Vec<&ProtectedStream> = entries.iter().collect();
        ordered.sort_by_key(|e| rank(e));

        let too_big = || CryptoError::invalid("the encryption stream would be over 4 GB");
        let mut out = vec![0u8; 8];
        let mut array = Vec::new();
        array.extend_from_slice(&(ordered.len() as u32).to_le_bytes());
        for (block, e) in ordered.iter().enumerate() {
            let units: Vec<u16> = e.name.encode_utf16().collect();
            if units.is_empty() || units.len() > usize::from(u8::MAX) || units.contains(&0) {
                return Err(CryptoError::invalid(
                    "every entry needs a name of 1 to 255 UTF-16 units without NUL",
                ));
            }
            let start = out.len();
            out.extend_from_slice(&e.data);
            rc4_block(&self.base, block as u32, &mut out[start..])?;
            array.extend_from_slice(&u32::try_from(start).map_err(|_| too_big())?.to_le_bytes());
            array.extend_from_slice(
                &u32::try_from(e.data.len())
                    .map_err(|_| too_big())?
                    .to_le_bytes(),
            );
            array.extend_from_slice(&(block as u16).to_le_bytes());
            array.push(units.len() as u8);
            array.push(u8::from(!e.storage)); // fStream
            array.extend_from_slice(&0u32.to_le_bytes()); // Reserved2
            for u in &units {
                array.extend_from_slice(&u.to_le_bytes());
            }
            array.extend_from_slice(&0u16.to_le_bytes()); // Reserved3
        }
        let offset = u32::try_from(out.len()).map_err(|_| too_big())?;
        let size = u32::try_from(array.len()).map_err(|_| too_big())?;
        u32::try_from(out.len() + array.len()).map_err(|_| too_big())?;
        rc4_block(&self.base, 0, &mut array)?;
        out.extend_from_slice(&array);
        out[..4].copy_from_slice(&offset.to_le_bytes());
        out[4..8].copy_from_slice(&size.to_le_bytes());
        rc4_block(&self.base, 0, &mut out[..8])?;
        Ok(out)
    }
}

/// A `\u{5}DocumentSummaryInformation` stream with no properties but the
/// code page (MS-OLEPS), which an `.xls` with encrypted document
/// properties has in place of the real one (MS-OFFCRYPTO 2.3.5.4).
pub fn empty_document_summary() -> Vec<u8> {
    // FMTID_DocSummaryInformation, D5CDD502-2E9C-101B-9397-08002B2CF9AE.
    const FMTID: [u8; 16] = [
        0x02, 0xD5, 0xCD, 0xD5, 0x9C, 0x2E, 0x1B, 0x10, 0x93, 0x97, 0x08, 0x00, 0x2B, 0x2C, 0xF9,
        0xAE,
    ];
    let mut out = Vec::with_capacity(72);
    out.extend_from_slice(&0xFFFEu16.to_le_bytes()); // ByteOrder
    out.extend_from_slice(&0u16.to_le_bytes()); // Version
    out.extend_from_slice(&0x0002_0000u32.to_le_bytes()); // SystemIdentifier: Win32
    out.extend_from_slice(&[0u8; 16]); // CLSID
    out.extend_from_slice(&1u32.to_le_bytes()); // NumPropertySets
    out.extend_from_slice(&FMTID);
    out.extend_from_slice(&48u32.to_le_bytes()); // Offset0
    // PropertySet: Size, NumProperties, then (PID_CODEPAGE, offset 16)
    // and a VT_I2 value of 1200 (UTF-16), padded to 4 bytes.
    out.extend_from_slice(&24u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1200u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// The FILEPASS body for RC4 CryptoAPI (MS-OFFCRYPTO 2.3.5.1): version
/// 4.2, the EncryptionHeader, and the verifier encrypted with the block 0
/// key. `fDocProps` is set (the summary streams stay plain) unless
/// `doc_props_encrypted`.
fn cryptoapi_filepass(
    base: &KeyBase,
    salt: &[u8],
    verifier: &[u8],
    doc_props_encrypted: bool,
) -> Result<Vec<u8>, CryptoError> {
    use ::rc4::StreamCipher;

    let flags = if doc_props_encrypted {
        F_CRYPTOAPI
    } else {
        F_CRYPTOAPI | F_DOCPROPS
    };
    let mut header = Vec::new();
    header.extend_from_slice(&flags.to_le_bytes()); // Flags
    header.extend_from_slice(&0u32.to_le_bytes()); // SizeExtra
    header.extend_from_slice(&0x6801u32.to_le_bytes()); // AlgID: RC4
    header.extend_from_slice(&0x8004u32.to_le_bytes()); // AlgIDHash: SHA-1
    header.extend_from_slice(&128u32.to_le_bytes()); // KeySize
    header.extend_from_slice(&1u32.to_le_bytes()); // ProviderType: PROV_RSA_FULL
    header.extend_from_slice(&0u32.to_le_bytes()); // Reserved1
    header.extend_from_slice(&0u32.to_le_bytes()); // Reserved2
    for u in CSP_NAME.encode_utf16().chain([0]) {
        header.extend_from_slice(&u.to_le_bytes());
    }

    let mut enc_verifier = Zeroizing::new(verifier.to_vec());
    let mut enc_hash = crate::util::HashAlgorithm::Sha1.digest(&[verifier]);
    let mut c = base.cipher(0)?;
    c.apply_keystream(&mut enc_verifier);
    c.apply_keystream(&mut enc_hash);

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_le_bytes()); // wEncryptionType: RC4
    out.extend_from_slice(&4u16.to_le_bytes()); // vMajor
    out.extend_from_slice(&2u16.to_le_bytes()); // vMinor
    out.extend_from_slice(&flags.to_le_bytes()); // Flags
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&16u32.to_le_bytes()); // SaltSize
    out.extend_from_slice(salt);
    out.extend_from_slice(&enc_verifier);
    out.extend_from_slice(&20u32.to_le_bytes()); // VerifierHashSize
    out.extend_from_slice(&enc_hash);
    Ok(out)
}
