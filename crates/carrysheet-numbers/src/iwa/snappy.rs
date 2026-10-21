//! Raw Snappy blocks, decoded from Google's format description
//! (`format_description.txt`, BSD licensed). No Snappy code was copied.

use super::read_varint;
use crate::container::Budget;

pub(super) enum SnappyError {
    Budget,
    Bad(&'static str),
}

/// The most one input byte can become: a 3-byte copy makes 64 bytes.
const MAX_EXPANSION: usize = 22;

/// Decompresses one raw Snappy block and appends it to `out`.
///
/// The length in the block header is checked before any memory is taken:
/// against what the block could possibly hold, and against the budget.
/// Every copy is checked against what was written so far, and all
/// arithmetic is checked, so a 32-bit target behaves like a 64-bit one.
pub(super) fn decompress(
    input: &[u8],
    out: &mut Vec<u8>,
    budget: &mut Budget,
) -> Result<(), SnappyError> {
    let (expected, mut pos) =
        read_varint(input).ok_or(SnappyError::Bad("the length header is bad"))?;
    if expected > u64::from(u32::MAX) {
        return Err(SnappyError::Bad("the length header is too big"));
    }
    let most = input.len().saturating_mul(MAX_EXPANSION);
    let expected = usize::try_from(expected)
        .ok()
        .filter(|&e| e <= most)
        .ok_or(SnappyError::Bad(
            "the length header is larger than the block can hold",
        ))?;
    budget
        .take(expected as u64)
        .map_err(|_| SnappyError::Budget)?;
    let start = out.len();
    out.try_reserve(expected).map_err(|_| SnappyError::Budget)?;
    let target = start
        .checked_add(expected)
        .ok_or(SnappyError::Bad("the length header is too big"))?;
    let cut = |what| SnappyError::Bad(what);
    while pos < input.len() {
        let tag = input[pos];
        pos += 1;
        let (len, offset) = match tag & 3 {
            0 => {
                let small = usize::from(tag >> 2);
                let len = if small < 60 {
                    small + 1
                } else {
                    let extra = small - 59;
                    let bytes = input
                        .get(pos..pos + extra)
                        .ok_or(cut("a literal is cut short"))?;
                    pos += extra;
                    let value = bytes
                        .iter()
                        .rev()
                        .fold(0u64, |acc, &b| acc << 8 | u64::from(b));
                    usize::try_from(value + 1).map_err(|_| cut("a literal is too long"))?
                };
                let end = pos.checked_add(len).ok_or(cut("a literal is cut short"))?;
                let bytes = input.get(pos..end).ok_or(cut("a literal is cut short"))?;
                if len > target - out.len() {
                    return Err(cut("the data is longer than its header says"));
                }
                out.extend_from_slice(bytes);
                pos = end;
                continue;
            }
            1 => {
                let low = *input.get(pos).ok_or(cut("a copy is cut short"))?;
                pos += 1;
                let len = usize::from((tag >> 2) & 7) + 4;
                (len, usize::from(tag >> 5) << 8 | usize::from(low))
            }
            2 => {
                let b = input.get(pos..pos + 2).ok_or(cut("a copy is cut short"))?;
                pos += 2;
                (
                    usize::from(tag >> 2) + 1,
                    usize::from(u16::from_le_bytes([b[0], b[1]])),
                )
            }
            _ => {
                let b = input.get(pos..pos + 4).ok_or(cut("a copy is cut short"))?;
                pos += 4;
                let offset = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                (
                    usize::from(tag >> 2) + 1,
                    usize::try_from(offset).map_err(|_| cut("a copy points before the start"))?,
                )
            }
        };
        let written = out.len() - start;
        if offset == 0 || offset > written {
            return Err(cut("a copy points before the start"));
        }
        if len > target - out.len() {
            return Err(cut("the data is longer than its header says"));
        }
        let from = out.len() - offset;
        if offset >= len {
            out.extend_from_within(from..from + len);
        } else {
            // Overlapping copy: repeats the last `offset` bytes.
            for i in 0..len {
                let byte = out[from + i];
                out.push(byte);
            }
        }
    }
    if out.len() != target {
        return Err(cut("the data is shorter than its header says"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> Result<Vec<u8>, &'static str> {
        let mut out = Vec::new();
        let mut budget = Budget::new(1 << 20);
        decompress(input, &mut out, &mut budget).map_err(|e| match e {
            SnappyError::Budget => "budget",
            SnappyError::Bad(d) => d,
        })?;
        Ok(out)
    }

    #[test]
    fn literal_and_copies() {
        // Literal "abcd", copy 4 bytes from 4 back, then an overlapping copy
        // of 5 bytes from 1 back.
        let block = [13, 0x0c, b'a', b'b', b'c', b'd', 0x01, 0x04, 0x05, 0x01];
        assert_eq!(run(&block).unwrap(), b"abcdabcdddddd");
    }

    #[test]
    fn rejects_bad_blocks() {
        assert!(run(&[]).is_err());
        assert!(run(&[5, 0x00, b'a']).is_err(), "too short");
        assert!(run(&[1, 0x04, b'a', b'b']).is_err(), "too long");
        assert!(run(&[4, 0x01, 0x01]).is_err(), "copy before start");
        assert!(run(&[0xff, 0xff, 0xff, 0xff, 0x7f]).is_err(), "huge header");
        // A literal whose 4-byte length is u32::MAX: on 32-bit targets the
        // length plus one overflows; it must be an error there too.
        assert!(run(&[10, 0xfc, 0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn header_larger_than_the_block_can_hold() {
        // 16 bytes cannot become more than 16 * 22.
        let mut block = vec![0x80, 0x80, 0x04];
        block.resize(16, 0);
        assert_eq!(
            run(&block),
            Err("the length header is larger than the block can hold")
        );
        // Within what the block could hold but over the budget.
        let mut block = vec![0x80, 0x80, 0x80, 0x01];
        block.resize(100_000, 0);
        assert_eq!(run(&block), Err("budget"));
    }
}
