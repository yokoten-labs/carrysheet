//! Estimates the memory a protobuf message takes once prost decodes it,
//! by walking the wire format first, without allocating.
//!
//! A message can be much larger in memory than on the wire: an empty
//! sub-message is two bytes on the wire but a whole struct (often hundreds
//! of bytes) in a `Vec`. So before an object is decoded, every field that
//! takes memory is charged: a sub-message its struct size, a string or byte
//! field its length, a repeated number 8 bytes. A repeated field is charged
//! as its `Vec` grows (at least four elements, then doubling, each new
//! buffer in full), and every heap allocation 32 bytes more for the
//! allocator. Fields the schema does not know are skipped by prost without
//! storing them, and cost nothing.
//!
//! The walk visits each byte once. When the data is malformed it stops and
//! leaves the error to prost, having charged what prost would have kept.

use super::read_varint;
use super::schema::MESSAGES;

/// A generated message type and its entry in [`MESSAGES`].
pub(crate) trait Described {
    const SCHEMA: usize;
}

/// How a field takes memory.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    /// A sub-message: the index of its schema.
    Message(usize),
    /// A string or bytes.
    Bytes,
    /// A repeated varint number (integers, booleans, enums).
    Varint,
    /// A repeated fixed-width number of this many bytes. The trimmed
    /// definitions have none today; codegen emits it when one is added.
    #[allow(dead_code)]
    Fixed(u8),
}

#[derive(Debug)]
pub(crate) struct Field {
    pub(crate) number: u32,
    pub(crate) kind: Kind,
    pub(crate) repeated: bool,
}

#[derive(Debug)]
pub(crate) struct Message {
    /// `size_of` the generated struct.
    pub(crate) size: usize,
    /// Fields that take memory, sorted by number.
    pub(crate) fields: &'static [Field],
}

/// prost's own limit on nested messages; deeper data fails to decode.
const MAX_DEPTH: u32 = 100;
/// The most fields of one message the schema lists; checked by a test.
const MAX_FIELDS: usize = 32;
/// Memory for one element of a repeated number (at most 8 bytes).
const NUMBER: u64 = 8;
/// A `String` or `Vec<u8>` element of a repeated field.
const STRING: u64 = 24;
/// What the allocator adds to each heap allocation, roughly.
const ALLOCATION: u64 = 32;

/// The memory, in bytes, that decoding `data` as message `schema` takes,
/// or `None` when it is more than `limit`.
pub(crate) fn estimate(data: &[u8], schema: usize, limit: u64) -> Option<u64> {
    let mut walk = Walk { total: 0, limit };
    walk.charge(MESSAGES[schema].size as u64)?;
    walk.message(data, schema, 0)?;
    Some(walk.total)
}

struct Walk {
    total: u64,
    limit: u64,
}

/// The capacity a `Vec` has after `n` pushes: 0, then 4, then doubling.
fn capacity(n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        n.next_power_of_two().max(4)
    }
}

impl Walk {
    fn charge(&mut self, bytes: u64) -> Option<()> {
        self.total = self.total.saturating_add(bytes);
        (self.total <= self.limit).then_some(())
    }

    /// Adds `added` elements of `size` bytes to a repeated field that had
    /// `*count`. Each time the `Vec` grows, its new buffer is charged in
    /// full, as the old one is still held while it is copied: about twice
    /// the final buffer in all, and never less than four elements.
    fn grow(&mut self, count: &mut u64, added: u64, size: u64) -> Option<()> {
        let before = capacity(*count);
        *count = count.saturating_add(added);
        let after = capacity(*count);
        if after > before {
            self.charge(after.saturating_mul(size).saturating_add(ALLOCATION))?;
        }
        Some(())
    }

    /// Walks one message. `None` means over the limit; malformed data just
    /// ends the walk.
    fn message(&mut self, mut data: &[u8], schema: usize, depth: u32) -> Option<()> {
        if depth > MAX_DEPTH {
            return Some(());
        }
        let fields = MESSAGES[schema].fields;
        // Elements so far of each repeated field, by its place in `fields`.
        let mut counts = [0u64; MAX_FIELDS];
        while !data.is_empty() {
            let Some((key, used)) = read_varint(data) else {
                return Some(());
            };
            data = &data[used..];
            let at = u32::try_from(key >> 3)
                .ok()
                .and_then(|number| fields.binary_search_by_key(&number, |f| f.number).ok())
                .filter(|&i| i < MAX_FIELDS);
            let skip = match key & 7 {
                0 => match read_varint(data) {
                    Some((_, used)) => used,
                    None => return Some(()),
                },
                1 => 8,
                5 => 4,
                2 => {
                    let Some((len, used)) = read_varint(data) else {
                        return Some(());
                    };
                    let Some(payload) = usize::try_from(len)
                        .ok()
                        .and_then(|len| data.get(used..used.checked_add(len)?))
                    else {
                        return Some(());
                    };
                    if let Some(i) = at {
                        self.field(&fields[i], &mut counts[i], payload, depth)?;
                    }
                    used + payload.len()
                }
                3 => match skip_group(data) {
                    Some(used) => used,
                    None => return Some(()),
                },
                _ => return Some(()),
            };
            // One number of a repeated field that is not packed.
            if key & 7 != 2
                && let Some(i) = at
                && fields[i].repeated
                && matches!(fields[i].kind, Kind::Varint | Kind::Fixed(_))
            {
                self.grow(&mut counts[i], 1, NUMBER)?;
            }
            let Some(rest) = data.get(skip..) else {
                return Some(());
            };
            data = rest;
        }
        Some(())
    }

    /// Charges one length-delimited field; `count` is how many elements a
    /// repeated field had before it.
    fn field(&mut self, field: &Field, count: &mut u64, payload: &[u8], depth: u32) -> Option<()> {
        let len = payload.len() as u64;
        let heap = if len > 0 { len + ALLOCATION } else { 0 };
        match field.kind {
            Kind::Message(child) => {
                let size = MESSAGES[child].size as u64;
                if field.repeated {
                    self.grow(count, 1, size)?;
                } else {
                    self.charge(size)?;
                }
                self.message(payload, child, depth + 1)
            }
            Kind::Bytes => {
                if field.repeated {
                    self.grow(count, 1, STRING)?;
                }
                self.charge(heap)
            }
            // A packed list of numbers.
            Kind::Varint if field.repeated => {
                let added = payload.iter().filter(|&&b| b < 0x80).count() as u64;
                self.grow(count, added, NUMBER)
            }
            Kind::Fixed(width) if field.repeated => {
                self.grow(count, len / u64::from(width.max(1)), NUMBER)
            }
            Kind::Varint | Kind::Fixed(_) => Some(()),
        }
    }
}

/// The length of a group's contents up to and including its end tag, or
/// `None` when the data ends first or nests too deep.
fn skip_group(data: &[u8]) -> Option<usize> {
    let mut pos = 0;
    let mut depth = 1u32;
    while depth > 0 {
        let (key, used) = read_varint(data.get(pos..)?)?;
        pos += used;
        let len = match key & 7 {
            0 => read_varint(data.get(pos..)?)?.1,
            1 => 8,
            5 => 4,
            2 => {
                let (len, used) = read_varint(data.get(pos..)?)?;
                used.checked_add(usize::try_from(len).ok()?)?
            }
            3 => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return None;
                }
                0
            }
            4 => {
                depth -= 1;
                0
            }
            _ => return None,
        };
        pos = pos.checked_add(len)?;
        if pos > data.len() {
            return None;
        }
    }
    Some(pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{tsp, tst};

    fn schema<M: Described>() -> usize {
        M::SCHEMA
    }

    /// What growing a `Vec` to `n` elements of `size` bytes is charged.
    fn grown(n: u64, size: u64) -> u64 {
        let mut total = 0;
        let mut cap = 4;
        loop {
            total += cap * size + ALLOCATION;
            if cap >= n {
                return total;
            }
            cap *= 2;
        }
    }

    #[test]
    fn empty_sub_messages_are_charged_their_size() {
        // 1000 empty rowInfos (field 5 of TST.Tile), two bytes each.
        let data = [0x2a, 0x00].repeat(1000);
        let size = size_of::<tst::Tile>() as u64;
        let row = size_of::<tst::TileRowInfo>() as u64;
        // Buffers of 4, 8, ..., 1024 rows: about twice the final one.
        assert_eq!(
            estimate(&data, schema::<tst::Tile>(), u64::MAX),
            Some(size + grown(1000, row))
        );
        assert!(grown(1000, row) >= 2 * 1000 * row);
        assert_eq!(estimate(&data, schema::<tst::Tile>(), 1000), None);
        // One element is charged a buffer of four.
        assert_eq!(
            estimate(&[0x2a, 0x00], schema::<tst::Tile>(), u64::MAX),
            Some(size + 4 * row + ALLOCATION)
        );
    }

    #[test]
    fn every_message_fits_the_field_counter() {
        assert!(MESSAGES.iter().all(|m| m.fields.len() <= MAX_FIELDS));
    }

    #[test]
    fn packed_numbers_and_unknown_fields() {
        // MessageInfo: packed object_references (field 5) of three numbers,
        // then an unknown field 99 holding a megabyte would be skipped.
        let mut data = vec![0x2a, 4, 1, 0x80, 0x01, 2];
        data.extend_from_slice(&[0x9a, 0x06, 3, b'a', b'b', b'c']);
        let size = size_of::<tsp::MessageInfo>() as u64;
        assert_eq!(
            estimate(&data, schema::<tsp::MessageInfo>(), u64::MAX),
            Some(size + 4 * NUMBER + ALLOCATION)
        );
    }

    #[test]
    fn malformed_data_ends_the_walk() {
        assert!(estimate(&[0x2a, 0xff], schema::<tst::Tile>(), u64::MAX).is_some());
        assert!(estimate(&[0xff; 12], schema::<tst::Tile>(), u64::MAX).is_some());
        // A group that never ends.
        assert!(estimate(&[0x0b; 300], schema::<tst::Tile>(), u64::MAX).is_some());
    }
}
