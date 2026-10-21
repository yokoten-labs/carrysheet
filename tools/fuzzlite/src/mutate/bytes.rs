//! Generic byte mutations that know nothing about the format.

use super::{INTERESTING_U16, INTERESTING_U32};
use crate::rng::Rng;

const INTERESTING_U8: &[u8] = &[0, 1, 0x7F, 0x80, 0xFF, 0x20, 0x0A, 0x0D, b'"', b','];

/// Applies one mutation and returns its name.
pub fn mutate(rng: &mut Rng, b: &mut Vec<u8>, other: &[u8], max_len: usize) -> &'static str {
    if b.is_empty() {
        let n = 1 + rng.below(64);
        b.extend((0..n).map(|_| rng.byte()));
        return "bytes:fill";
    }
    let len = b.len();
    let at = rng.below(len);
    match rng.below(13) {
        0 | 1 => {
            b[at] ^= 1 << rng.below(8);
            "bytes:flip"
        }
        2 => {
            b[at] = rng.pick(INTERESTING_U8);
            "bytes:set8"
        }
        3 => {
            let v = rng.pick(INTERESTING_U16);
            let v = if rng.one_in(4) { v.swap_bytes() } else { v };
            put(b, at, &v.to_le_bytes());
            "bytes:set16"
        }
        4 => {
            let v = rng.pick(INTERESTING_U32);
            let v = if rng.one_in(4) { v.swap_bytes() } else { v };
            put(b, at, &v.to_le_bytes());
            "bytes:set32"
        }
        5 => {
            // Nudge a little-endian number: off-by-one and small overflows.
            let delta = 1 + rng.below(16) as u32;
            let width = rng.pick(&[1usize, 2, 4]);
            if let Some(slot) = b.get_mut(at..at + width) {
                let mut v = [0u8; 4];
                v[..width].copy_from_slice(slot);
                let n = u32::from_le_bytes(v);
                let n = if rng.one_in(2) {
                    n.wrapping_add(delta)
                } else {
                    n.wrapping_sub(delta)
                };
                slot.copy_from_slice(&n.to_le_bytes()[..width]);
            }
            "bytes:arith"
        }
        6 => {
            let n = 1 + rng.below(32);
            let fill: Vec<u8> = if rng.one_in(2) {
                (0..n).map(|_| rng.byte()).collect()
            } else {
                vec![rng.pick(INTERESTING_U8); n]
            };
            insert(b, at, &fill, max_len);
            "bytes:insert"
        }
        7 => {
            let end = (at + 1 + rng.below(len.min(4096))).min(len);
            b.drain(at..end);
            "bytes:delete"
        }
        8 => {
            // Duplicate a range, sometimes many times: repeated records and
            // repeated structures are how small files make big work.
            let end = (at + 1 + rng.below(len.min(1024))).min(len);
            let piece = b[at..end].to_vec();
            let times = rng.count(max_len / piece.len().max(1));
            let to = rng.below(len + 1);
            let copies = piece.repeat(times);
            insert(b, to, &copies, max_len);
            "bytes:dup"
        }
        9 => {
            // Copy a range over another place.
            let end = (at + 1 + rng.below(len.min(256))).min(len);
            let piece = b[at..end].to_vec();
            let to = rng.below(len);
            put(b, to, &piece);
            "bytes:copy"
        }
        10 => {
            b.truncate(at);
            "bytes:truncate"
        }
        11 => {
            // Splice: this input's head with the other input's tail, or a
            // piece of the other input inserted here.
            if other.is_empty() {
                b[at] = rng.byte();
            } else if rng.one_in(2) {
                let from = rng.below(other.len());
                b.truncate(at);
                b.extend_from_slice(&other[from..]);
            } else {
                let from = rng.below(other.len());
                let end = (from + 1 + rng.below(4096)).min(other.len());
                insert(b, at, &other[from..end], max_len);
            }
            "bytes:splice"
        }
        _ => {
            // Overwrite a short run with one value.
            let end = (at + 1 + rng.below(64)).min(len);
            let v = rng.pick(INTERESTING_U8);
            b[at..end].fill(v);
            "bytes:run"
        }
    }
}

/// Overwrites bytes at `at`, clipped to the end.
fn put(b: &mut [u8], at: usize, value: &[u8]) {
    let end = (at + value.len()).min(b.len());
    if at < end {
        b[at..end].copy_from_slice(&value[..end - at]);
    }
}

/// Inserts bytes at `at`, keeping the result within `max_len`.
pub fn insert(b: &mut Vec<u8>, at: usize, value: &[u8], max_len: usize) {
    let room = max_len.saturating_sub(b.len());
    let value = &value[..value.len().min(room)];
    let at = at.min(b.len());
    b.splice(at..at, value.iter().copied());
}
