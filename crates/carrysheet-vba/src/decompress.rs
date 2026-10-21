//! The MS-OVBA run-length compression (section 2.4.1), decompression only.
//!
//! A compressed container is a signature byte `0x01` followed by chunks.
//! Each chunk has a two-byte header and decompresses to at most 4096 bytes,
//! either stored raw or as token sequences of literals and copy tokens that
//! point back into the same decompressed chunk.

use crate::error::VbaError;

/// Bytes one chunk decompresses to at most.
const CHUNK_SIZE: usize = 4096;

fn bad(offset: usize, detail: impl Into<String>) -> VbaError {
    VbaError::BadCompression {
        stream: String::new(),
        offset,
        detail: detail.into(),
    }
}

/// Decompresses one MS-OVBA compressed container.
///
/// `max_output` caps the decompressed size: a few compressed bytes can
/// expand to 4096, so a hostile container must not be allowed to fill
/// memory. Reaching the cap is a [`VbaError::LimitExceeded`] error.
///
/// ```
/// // MS-OVBA 3.2.1, "No Compression Example".
/// let packed = [
///     0x01, 0x19, 0xB0, 0x00, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x00,
///     0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x00, 0x71, 0x72, 0x73, 0x74,
///     0x75, 0x76, 0x2E,
/// ];
/// let text = carrysheet_vba::decompress(&packed, 1 << 20)?;
/// assert_eq!(text, b"abcdefghijklmnopqrstuv.");
/// # Ok::<(), carrysheet_vba::VbaError>(())
/// ```
pub fn decompress(input: &[u8], max_output: usize) -> Result<Vec<u8>, VbaError> {
    let mut out = Vec::new();
    decompress_into(input, max_output, &mut out).map(|()| out)
}

/// Like [`decompress`], and also returns how many bytes were produced
/// before success or failure, so callers can count the work done.
pub(crate) fn decompress_counted(
    input: &[u8],
    max_output: usize,
) -> (Result<Vec<u8>, VbaError>, usize) {
    let mut out = Vec::new();
    let result = decompress_into(input, max_output, &mut out);
    let produced = out.len();
    (result.map(|()| out), produced)
}

fn decompress_into(input: &[u8], max_output: usize, out: &mut Vec<u8>) -> Result<(), VbaError> {
    let Some(&signature) = input.first() else {
        return Err(bad(0, "the compressed container is empty"));
    };
    if signature != 0x01 {
        return Err(bad(
            0,
            format!("the container starts with 0x{signature:02X}, not the signature byte 0x01"),
        ));
    }
    // Real containers expand two to four times; reserve modestly and let
    // the vector grow only as data actually arrives.
    out.reserve(input.len().saturating_mul(2).min(max_output));
    let mut pos = 1;
    while pos < input.len() {
        let chunk_start = pos;
        let Some(header) = input.get(pos..pos + 2) else {
            return Err(bad(pos, "a chunk header is cut off at the end of the data"));
        };
        let header = u16::from_le_bytes([header[0], header[1]]);
        let size = usize::from(header & 0x0FFF) + 3;
        let signature = (header >> 12) & 0b111;
        if signature != 0b011 {
            return Err(bad(
                pos,
                format!("the chunk header signature is 0b{signature:03b}, not 0b011"),
            ));
        }
        let compressed = header & 0x8000 != 0;
        let end = input.len().min(chunk_start + size);
        pos += 2;
        let chunk_out_start = out.len();
        if compressed {
            while pos < end {
                let flags = input[pos];
                pos += 1;
                for bit in 0..8 {
                    if pos >= end {
                        break;
                    }
                    let written = out.len() - chunk_out_start;
                    if flags & (1 << bit) == 0 {
                        if written >= CHUNK_SIZE {
                            return Err(bad(pos, "a chunk decompresses to more than 4096 bytes"));
                        }
                        if out.len() >= max_output {
                            return Err(VbaError::limit(
                                "decompressed bytes per stream",
                                max_output,
                            ));
                        }
                        out.push(input[pos]);
                        pos += 1;
                    } else {
                        let Some(token) = input.get(pos..pos + 2).filter(|_| pos + 2 <= end) else {
                            return Err(bad(
                                pos,
                                "a copy token is cut off at the end of its chunk",
                            ));
                        };
                        let token = u16::from_le_bytes([token[0], token[1]]);
                        let (offset, length) =
                            unpack_copy_token(token, written).ok_or_else(|| {
                                bad(pos, "a copy token comes before any data in its chunk")
                            })?;
                        if offset > written {
                            return Err(bad(
                                pos,
                                format!(
                                    "a copy token points {offset} bytes back, but its chunk has only {written}"
                                ),
                            ));
                        }
                        if written + length > CHUNK_SIZE {
                            return Err(bad(pos, "a chunk decompresses to more than 4096 bytes"));
                        }
                        if out.len() + length > max_output {
                            return Err(VbaError::limit(
                                "decompressed bytes per stream",
                                max_output,
                            ));
                        }
                        // The copy may overlap what it writes (offset < length);
                        // it then repeats the last `offset` bytes. Copying
                        // everything from `start` keeps that period while the
                        // copied run doubles each step.
                        let start = out.len() - offset;
                        let mut remaining = length;
                        while remaining > 0 {
                            let n = (out.len() - start).min(remaining);
                            out.extend_from_within(start..start + n);
                            remaining -= n;
                        }
                        pos += 2;
                    }
                }
            }
        } else {
            // A raw chunk holds 4096 bytes as they are. Tolerate a short last
            // chunk: some writers end the data early.
            let n = CHUNK_SIZE.min(input.len() - pos);
            if out.len() + n > max_output {
                return Err(VbaError::limit("decompressed bytes per stream", max_output));
            }
            out.extend_from_slice(&input[pos..pos + n]);
            pos += n;
        }
    }
    Ok(())
}

/// Splits a copy token into (offset, length), MS-OVBA 2.4.1.3.19.
///
/// The split depends on how much of the chunk is already decompressed:
/// the further in, the more bits go to the offset. Returns `None` when
/// nothing has been decompressed yet, since there is nothing to copy.
fn unpack_copy_token(token: u16, written: usize) -> Option<(usize, usize)> {
    if written == 0 {
        return None;
    }
    // Smallest bit count with 2^bits >= written, at least 4. `written` is
    // at most 4096 here, so this stays at or below 12.
    let mut bit_count = 4u32;
    while (1usize << bit_count) < written {
        bit_count += 1;
    }
    let length_mask = 0xFFFFu16 >> bit_count;
    let length = usize::from(token & length_mask) + 3;
    let offset = usize::from(token >> (16 - bit_count)) + 1;
    Some((offset, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte-by-byte copy the spec describes.
    fn naive(packed: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut pos = 1;
        while pos + 2 <= packed.len() {
            let header = u16::from_le_bytes([packed[pos], packed[pos + 1]]);
            let end = packed.len().min(pos + usize::from(header & 0xFFF) + 3);
            pos += 2;
            let chunk = out.len();
            while pos < end {
                let flags = packed[pos];
                pos += 1;
                for bit in 0..8 {
                    if pos >= end {
                        break;
                    }
                    if flags & (1 << bit) == 0 {
                        out.push(packed[pos]);
                        pos += 1;
                    } else {
                        let token = u16::from_le_bytes([packed[pos], *packed.get(pos + 1)?]);
                        let (offset, length) = unpack_copy_token(token, out.len() - chunk)?;
                        for _ in 0..length {
                            out.push(out[out.len() - offset]);
                        }
                        pos += 2;
                    }
                }
            }
        }
        Some(out)
    }

    #[test]
    fn overlapping_copies_match_the_byte_by_byte_copy() {
        // After `lit` literals, copy `len` bytes from `off` back.
        for lit in 1..=9u8 {
            for off in 1..=lit as usize {
                for len in [3usize, 4, 7, 15, 16, 17, 100, 4096 - 9 - 1] {
                    let mut packed = vec![0x01, 0, 0];
                    let mut body = Vec::new();
                    let mut flags_at = None;
                    for i in 0..lit {
                        if i % 8 == 0 {
                            flags_at = Some(body.len());
                            body.push(0);
                        }
                        body.push(b'a' + i);
                    }
                    if lit % 8 == 0 {
                        flags_at = Some(body.len());
                        body.push(0);
                    }
                    let f = flags_at.unwrap();
                    body[f] |= 1 << (lit % 8);
                    let written = usize::from(lit);
                    let mut bits = 4;
                    while (1usize << bits) < written {
                        bits += 1;
                    }
                    let token = (((off - 1) as u16) << (16 - bits)) | (len - 3) as u16;
                    body.extend_from_slice(&token.to_le_bytes());
                    let header = 0xB000u16 | (body.len() as u16 + 2 - 3);
                    packed[1..3].copy_from_slice(&header.to_le_bytes());
                    packed.extend_from_slice(&body);
                    assert_eq!(
                        decompress(&packed, 1 << 20).ok(),
                        naive(&packed),
                        "lit {lit} off {off} len {len}"
                    );
                }
            }
        }
    }

    #[test]
    fn copy_token_bit_split_follows_the_table() {
        // 1 to 16 bytes written: 12 length bits, 4 offset bits.
        assert_eq!(unpack_copy_token(0x0045, 1), Some((1, 0x45 + 3)));
        // 17 to 32: 11 length bits.
        assert_eq!(unpack_copy_token(0b00001_00000000011, 17), Some((2, 6)));
        // 2049 to 4096: 4 length bits, 12 offset bits.
        assert_eq!(unpack_copy_token(0xFFFF, 4096), Some((4096, 18)));
        assert_eq!(unpack_copy_token(0x1234, 0), None);
    }
}
