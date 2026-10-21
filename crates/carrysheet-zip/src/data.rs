//! Entry data: the local header (APPNOTE 4.3.7), then Stored or Deflate
//! data (4.4.5), with every byte in and out on a budget.

use std::io::{Read, Seek, SeekFrom};

use flate2::{Crc, Decompress, FlushDecompress, Status};

use crate::directory::{LOCAL_LEN, Zip64Fields, le16, le32};
use crate::error::{Error, damaged, limit};
use crate::{
    Archive, Entry, FLAG_DATA_DESCRIPTOR, FLAG_ENCRYPTED, FLAG_STRONG_ENCRYPTION, Interrupt,
    Limits, Method,
};

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
/// Input read per step when inflating a whole entry.
const CHUNK: usize = 64 << 10;
/// The first input step when only a prefix is wanted: a deflate stream's
/// first bytes come out after a few hundred bytes of input.
const FIRST_PREFIX_CHUNK: usize = 512;
/// Input handed to the inflater per step. Between steps the progress
/// guard and the caller's interrupt are checked, so no step runs long.
const STEP: usize = 512;
/// How far the input consumed may run ahead of the output produced: a
/// dynamic block header (under 300 bytes) and the inflater's look-ahead,
/// with room to spare. On top, a share of the output, because valid
/// deflate can be larger than the bytes it codes: fixed-Huffman literals
/// 144 to 255 take 9 bits (12.5% more), and zlib-rs's fastest level grows
/// 3 MiB of random bytes by 5.5% (miniz's by 0.09%). A quarter is the
/// same allowance `read` gives the sizes in the directory. Empty blocks
/// produce nothing, so they are refused after the slack; filler after
/// real output can only use the room that output earned, and the
/// caller's interrupt bounds the time that takes.
const PROGRESS_SLACK: u64 = 1024;
const PROGRESS_SHARE: u64 = 4;

pub(crate) fn read<R: Read + Seek>(
    a: &mut Archive<R>,
    index: usize,
    max_bytes: u64,
) -> Result<Vec<u8>, Error> {
    let entry = entry(a, index)?;
    let max = max_bytes.min(a.limits.max_entry_bytes);
    if entry.size > max {
        return Err(limit("bytes in one entry", max));
    }
    if a.out_total.saturating_add(entry.size) > a.limits.max_total_bytes {
        return Err(limit(
            "bytes read from all entries",
            a.limits.max_total_bytes,
        ));
    }
    match entry.method {
        Method::Stored if entry.compressed_size != entry.size => {
            return Err(damaged(format!(
                "entry {index} is stored, but its two sizes differ"
            )));
        }
        // Deflate from ordinary writers grows data by a few percent at
        // most (stored blocks add 5 bytes per 65,535). A stream far
        // larger than its output is padding, such as empty blocks, made
        // to burn time.
        // Valid deflate can trip this too: a writer that sync-flushes
        // after every byte or two adds 5 bytes per flush. The check
        // stays (it is informational), and its message says a valid but
        // unusual writer can cause it.
        Method::Deflated
            if entry.compressed_size > entry.size.saturating_add(entry.size / 4 + 1024) =>
        {
            return Err(damaged(format!(
                "entry {index}'s compressed data is far larger than its output, which is how padding made to waste time looks; a valid but unusual writer (such as one that flushes after every few bytes) can also cause it"
            )));
        }
        _ => {}
    }
    let start = data_start(&mut a.reader, &entry, index)?;
    // Charged before anything is read or inflated.
    charge(
        &mut a.read_total,
        &a.limits,
        entry.compressed_size,
        &mut 0,
        u64::MAX,
    )?;
    a.reader.seek(SeekFrom::Start(start))?;
    let out = match entry.method {
        Method::Deflated => inflate(
            &mut a.reader,
            entry.compressed_size,
            CHUNK,
            Want::Whole(entry.size),
            |_| Ok(()),
            &mut a.interrupt,
        )?,
        _ => {
            // Bounded: the size is within the limit and inside the file.
            let mut out = vec![0u8; entry.size as usize];
            a.reader.read_exact(&mut out)?;
            out
        }
    };
    if out.len() as u64 != entry.size {
        return Err(damaged(format!(
            "entry {index} inflates to a different size than it claims"
        )));
    }
    let mut crc = Crc::new();
    crc.update(&out);
    if crc.sum() != entry.crc32 {
        return Err(damaged(format!("entry {index} fails its CRC-32 check")));
    }
    a.out_total += out.len() as u64;
    Ok(out)
}

pub(crate) fn read_prefix<R: Read + Seek>(
    a: &mut Archive<R>,
    index: usize,
    n: usize,
) -> Result<Vec<u8>, Error> {
    let entry = entry(a, index)?;
    // Asking for the whole entry: read it, with every check `read` makes.
    if n as u64 >= entry.size {
        return read(a, index, a.limits.max_entry_bytes);
    }
    // Shorter than the entry, so it fits in memory; not allocated up front.
    let n = n.min(usize::try_from(a.limits.max_entry_bytes).unwrap_or(usize::MAX));
    if a.out_total.saturating_add(n as u64) > a.limits.max_total_bytes {
        return Err(limit(
            "bytes read from all entries",
            a.limits.max_total_bytes,
        ));
    }
    let start = data_start(&mut a.reader, &entry, index)?;
    a.reader.seek(SeekFrom::Start(start))?;
    let Archive {
        reader,
        limits,
        read_total,
        interrupt,
        ..
    } = a;
    let mut spent = 0;
    let out = match entry.method {
        Method::Deflated => inflate(
            reader,
            entry.compressed_size,
            FIRST_PREFIX_CHUNK,
            Want::Prefix(n),
            |k| charge(read_total, limits, k, &mut spent, limits.max_prefix_read),
            interrupt,
        )?,
        _ => {
            let k = n.min(usize::try_from(entry.compressed_size).unwrap_or(usize::MAX));
            charge(
                read_total,
                limits,
                k as u64,
                &mut spent,
                limits.max_prefix_read,
            )?;
            let mut out = vec![0u8; k];
            reader.read_exact(&mut out)?;
            out
        }
    };
    a.out_total += out.len() as u64;
    Ok(out)
}

/// The entry, if it can be read at all.
fn entry<R>(a: &Archive<R>, index: usize) -> Result<Entry, Error> {
    let Some(entry) = a.entries.get(index) else {
        return Err(Error::Unsupported(format!("there is no entry {index}")));
    };
    if entry.is_encrypted() {
        return Err(Error::Unsupported(format!("entry {index} is encrypted")));
    }
    if let Method::Other(code) = entry.method {
        return Err(Error::Unsupported(format!(
            "entry {index} uses compression method {code}; only Stored (0) and Deflate (8) are read"
        )));
    }
    Ok(entry.clone())
}

/// Charges `k` compressed bytes to the total budget and to one call's.
fn charge(
    total: &mut u64,
    limits: &Limits,
    k: u64,
    spent: &mut u64,
    call_max: u64,
) -> Result<(), Error> {
    *spent = spent.saturating_add(k);
    if *spent > call_max {
        return Err(limit(
            "compressed bytes read for one entry's first bytes",
            call_max,
        ));
    }
    *total = total.saturating_add(k);
    if *total > limits.max_total_read {
        return Err(limit(
            "compressed bytes read from the archive",
            limits.max_total_read,
        ));
    }
    Ok(())
}

/// Reads and checks the local header, and returns where the data starts.
fn data_start<R: Read + Seek>(reader: &mut R, entry: &Entry, index: usize) -> Result<u64, Error> {
    let mismatch = |what: &str| {
        damaged(format!(
            "entry {index}'s local header disagrees with the central directory ({what})"
        ))
    };
    reader.seek(SeekFrom::Start(entry.header_offset))?;
    let mut h = [0u8; LOCAL_LEN as usize];
    reader.read_exact(&mut h)?;
    if le32(&h, 0) != LOCAL_SIGNATURE {
        return Err(damaged(format!(
            "entry {index}'s local header is not where the directory says"
        )));
    }
    let flags = le16(&h, 6);
    let method = le16(&h, 8);
    let crc32 = le32(&h, 14);
    let mut compressed_size = u64::from(le32(&h, 18));
    let mut size = u64::from(le32(&h, 22));
    let name_len = usize::from(le16(&h, 26));
    let extra_len = usize::from(le16(&h, 28));
    if name_len != entry.raw_name.len() {
        return Err(mismatch("name"));
    }
    let mut name_and_extra = vec![0u8; name_len + extra_len];
    reader.read_exact(&mut name_and_extra)?;
    let (name, extra) = name_and_extra.split_at(name_len);
    if *name != *entry.raw_name {
        return Err(mismatch("name"));
    }
    if method != method_code(entry.method) {
        return Err(mismatch("compression method"));
    }
    // Encryption must agree. The data-descriptor flag may differ (some
    // writers set it only in the directory): the sizes then must agree.
    let checked = FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION;
    if (flags ^ entry.flags) & checked != 0 {
        return Err(mismatch("flags"));
    }
    let deferred = flags & FLAG_DATA_DESCRIPTOR != 0;
    // APPNOTE 4.5.3: when either local size is all ones, the zip64 extra
    // field holds both (size, then compressed size). Writers that give
    // only the flagged one are read too.
    let all_ones = u64::from(u32::MAX);
    if size == all_ones || compressed_size == all_ones {
        let (mut both_size, mut both_compressed) = (size, compressed_size);
        let mut both = Zip64Fields {
            size: Some(&mut both_size),
            compressed_size: Some(&mut both_compressed),
            header_offset: None,
            disk: None,
        };
        if both.read(extra) && extra_holds_both(extra) {
            (size, compressed_size) = (both_size, both_compressed);
        } else {
            let mut fields = Zip64Fields {
                size: (size == all_ones).then_some(&mut size),
                compressed_size: (compressed_size == all_ones).then_some(&mut compressed_size),
                header_offset: None,
                disk: None,
            };
            // With a data descriptor the local sizes are placeholders
            // (some streaming writers put all ones and no extra field).
            if !fields.read(extra) && !deferred {
                return Err(mismatch("zip64 sizes"));
            }
        }
    }
    // With a data descriptor the local values may be left as zero (or
    // all ones); the directory's are the real ones.
    let agrees = |local: u64, central: u64| {
        local == central || (deferred && (local == 0 || local == all_ones))
    };
    if !agrees(compressed_size, entry.compressed_size)
        || !agrees(size, entry.size)
        || !agrees(u64::from(crc32), u64::from(entry.crc32))
    {
        return Err(mismatch("sizes or CRC-32"));
    }
    let start = entry.header_offset + LOCAL_LEN + (name_len + extra_len) as u64;
    if start
        .checked_add(entry.compressed_size)
        .is_none_or(|end| end > entry.data_limit)
    {
        return Err(damaged(format!(
            "entry {index}'s data runs into the next entry"
        )));
    }
    Ok(start)
}

/// True when the zip64 extra field has room for two 8-byte values.
fn extra_holds_both(extra: &[u8]) -> bool {
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = le16(extra, at);
        let len = usize::from(le16(extra, at + 2));
        if id == 1 {
            return len >= 16;
        }
        at += 4 + len;
    }
    false
}

fn method_code(method: Method) -> u16 {
    match method {
        Method::Stored => 0,
        Method::Deflated => 8,
        Method::Other(code) => code,
    }
}

enum Want {
    /// The whole stream, which must inflate to exactly this many bytes.
    Whole(u64),
    /// Only the first bytes.
    Prefix(usize),
}

/// Inflates a raw deflate stream of `compressed` bytes from `reader`.
/// `charge` is called with each input chunk's size before it is read.
///
/// Work is bounded by the output, not only the input: some inflaters
/// spend microseconds on each empty block (10 bits for a fixed-Huffman
/// one), so a stream of them costs seconds per megabyte while producing
/// nothing. Real deflate never consumes much input without output, so
/// the input consumed may lead the output by [`PROGRESS_SLACK`] (plus a
/// 1/[`PROGRESS_SHARE`] share of the output) and no more, checked every
/// [`STEP`] bytes.
fn inflate<R: Read>(
    reader: &mut R,
    compressed: u64,
    first_chunk: usize,
    want: Want,
    mut charge: impl FnMut(u64) -> Result<(), Error>,
    interrupt: &mut Option<Interrupt>,
) -> Result<Vec<u8>, Error> {
    let (cap, initial) = match want {
        Want::Whole(size) => {
            let size = usize::try_from(size).unwrap_or(usize::MAX);
            (size, size.min(8 << 20))
        }
        Want::Prefix(n) => (n, n.min(64 << 10)),
    };
    let mut out = Vec::with_capacity(initial);
    if cap == 0 && matches!(want, Want::Prefix(_)) {
        return Ok(out);
    }
    let mut dec = Decompress::new(false);
    let mut input = Vec::new();
    let mut pos = 0;
    let mut left = compressed;
    let mut chunk = first_chunk;
    let mut buf = vec![0u8; 32 << 10];
    loop {
        // With every input byte consumed, the inflater may still hold
        // output that did not fit in `buf` (a tail that inflates at a high
        // ratio, such as a run of zeros): it is drained with empty input
        // below, and only a call that makes nothing and does not end the
        // stream shows the data is cut short.
        if pos == input.len() && left > 0 {
            let k = (chunk as u64).min(left) as usize;
            charge(k as u64)?;
            input.resize(k, 0);
            reader.read_exact(&mut input)?;
            pos = 0;
            left -= k as u64;
            chunk = (chunk * 2).min(CHUNK);
        }
        // One byte past the cap shows that a stream says too little.
        let room = (cap - out.len()).saturating_add(1).min(buf.len());
        let (in0, out0) = (dec.total_in(), dec.total_out());
        let step = (input.len() - pos).min(STEP);
        let status = dec
            .decompress(
                &input[pos..pos + step],
                &mut buf[..room],
                FlushDecompress::None,
            )
            .map_err(|_| damaged("an entry's deflate data is not valid"))?;
        let used = (dec.total_in() - in0) as usize;
        let made = (dec.total_out() - out0) as usize;
        pos += used;
        let (total_in, total_out) = (dec.total_in(), dec.total_out());
        if total_in > total_out + total_out / PROGRESS_SHARE + PROGRESS_SLACK {
            return Err(damaged(
                "an entry's deflate data consumes input without producing output (such as runs of empty blocks)",
            ));
        }
        if let Some(stop) = interrupt.as_mut()
            && stop()
        {
            return Err(Error::Interrupted);
        }
        if out.len() + made > cap {
            if let Want::Prefix(n) = want {
                out.extend_from_slice(&buf[..n - out.len()]);
                return Ok(out);
            }
            return Err(damaged("an entry inflates to more bytes than it claims"));
        }
        out.extend_from_slice(&buf[..made]);
        match want {
            Want::Prefix(n) if out.len() >= n => return Ok(out),
            _ if status == Status::StreamEnd => return Ok(out),
            _ => {}
        }
        if used == 0 && made == 0 {
            if pos < input.len() {
                return Err(damaged("an entry's deflate data stops making progress"));
            }
            if left == 0 {
                return Err(damaged(
                    "an entry's compressed data ends before its deflate stream does",
                ));
            }
        }
    }
}
