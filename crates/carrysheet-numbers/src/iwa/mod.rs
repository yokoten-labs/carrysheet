//! IWA archives: chunk framing, Snappy blocks, and objects by identifier.
//!
//! An `.iwa` file is a list of chunks. Each chunk is a zero byte, a 24-bit
//! little-endian length, and that many bytes of one raw Snappy block (no
//! Snappy framing format, no CRC). The decompressed chunks, joined, are a
//! list of archives: a varint length, a `TSP.ArchiveInfo` message, and then
//! the messages it describes, one after another. The first message of an
//! archive is the object with the archive's identifier.

mod schema;
mod snappy;
mod wire;

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;

use prost::Message;

use crate::container::{Budget, Limits, Part};
use crate::error::{NumbersError, damaged};
use crate::proto::tsp;
use snappy::SnappyError;
pub(crate) use wire::Described;

/// The largest archive header (`TSP.ArchiveInfo`). Real ones are a few
/// dozen bytes.
const MAX_HEADER: u64 = 64 << 10;
/// The most memory one archive header may take once decoded.
const MAX_HEADER_MEMORY: u64 = 16 << 20;

/// Where an object's message bytes are.
#[derive(Clone, Debug)]
struct Entry {
    part: usize,
    range: Range<usize>,
    kind: u32,
}

/// Every object in the document, by identifier.
pub(crate) struct Store {
    names: Vec<String>,
    data: Vec<Vec<u8>>,
    objects: HashMap<u64, Entry>,
    /// Identifiers in the order the objects appear in the package.
    order: Vec<u64>,
    /// Bytes of objects decoded so far, and the most allowed. Objects can
    /// be referred to many times; this bounds the work of decoding them.
    decoded: Cell<u64>,
    decode_limit: u64,
    /// Memory taken by decoded objects so far (estimated before each
    /// decode; see `wire`), and the most allowed.
    memory: Cell<u64>,
    memory_limit: u64,
}

impl Store {
    pub(crate) fn load(
        parts: Vec<Part>,
        limits: &Limits,
        budget: &mut Budget,
    ) -> Result<Self, NumbersError> {
        let mut store = Self {
            names: Vec::with_capacity(parts.len()),
            data: Vec::with_capacity(parts.len()),
            objects: HashMap::new(),
            order: Vec::new(),
            decoded: Cell::new(0),
            decode_limit: limits.max_total_bytes.saturating_mul(2),
            memory: Cell::new(0),
            memory_limit: limits.max_decoded_memory,
        };
        for part in parts {
            let index = store.data.len();
            let data = decompress_chunks(&part.name, &part.data, budget)?;
            drop(part.data);
            store.index_archives(index, &part.name, &data, limits)?;
            store.names.push(part.name);
            store.data.push(data);
        }
        Ok(store)
    }

    fn index_archives(
        &mut self,
        part: usize,
        name: &str,
        data: &[u8],
        limits: &Limits,
    ) -> Result<(), NumbersError> {
        let mut pos = 0;
        while pos < data.len() {
            let (len, used) = read_varint(&data[pos..])
                .ok_or_else(|| damaged(name, "an archive header is cut short"))?;
            pos += used;
            if len > MAX_HEADER {
                return Err(NumbersError::TooLarge {
                    what: "bytes in one archive header",
                    limit: MAX_HEADER,
                });
            }
            let end = checked_end(pos, len, data.len())
                .ok_or_else(|| damaged(name, "an archive header is longer than the data"))?;
            let header = &data[pos..end];
            if wire::estimate(header, tsp::ArchiveInfo::SCHEMA, MAX_HEADER_MEMORY).is_none() {
                return Err(NumbersError::TooLarge {
                    what: "bytes of memory for one archive header",
                    limit: MAX_HEADER_MEMORY,
                });
            }
            let info = tsp::ArchiveInfo::decode(header)
                .map_err(|e| damaged(name, format!("an archive header does not decode ({e})")))?;
            pos = end;
            let id = info.identifier.unwrap_or(0);
            for (i, message) in info.message_infos.iter().enumerate() {
                let length = u64::from(message.length);
                if length > limits.max_object_bytes {
                    return Err(NumbersError::TooLarge {
                        what: "bytes in one object",
                        limit: limits.max_object_bytes,
                    });
                }
                let end = checked_end(pos, length, data.len())
                    .ok_or_else(|| damaged(name, format!("object {id} is longer than the data")))?;
                // Only the first message is the object itself; the others
                // are merge patches that this reader does not apply.
                if i == 0 && id != 0 {
                    if self.objects.len() >= limits.max_objects {
                        return Err(NumbersError::TooLarge {
                            what: "objects",
                            limit: limits.max_objects as u64,
                        });
                    }
                    let entry = Entry {
                        part,
                        range: pos..end,
                        kind: message.r#type,
                    };
                    // A later copy of an object replaces an earlier one.
                    if self.objects.insert(id, entry).is_none() {
                        self.order.push(id);
                    }
                }
                pos = end;
            }
        }
        Ok(())
    }

    /// The type number of an object, if it exists.
    pub(crate) fn kind(&self, id: u64) -> Option<u32> {
        self.objects.get(&id).map(|e| e.kind)
    }

    /// Identifiers of all objects of one of the given types, in package order.
    pub(crate) fn ids_of_kind<'a>(&'a self, kinds: &'a [u32]) -> impl Iterator<Item = u64> + 'a {
        self.order
            .iter()
            .copied()
            .filter(move |id| self.kind(*id).is_some_and(|k| kinds.contains(&k)))
    }

    /// Decodes an object, checking that it exists and has the expected type,
    /// and charging the work and the memory it takes.
    pub(crate) fn get<M: Message + Default + Described>(
        &self,
        id: u64,
        ty: &ObjectType,
        needed_for: impl FnOnce() -> String,
    ) -> Result<M, NumbersError> {
        let entry = self
            .objects
            .get(&id)
            .ok_or_else(|| NumbersError::MissingObject {
                id,
                needed_for: needed_for(),
            })?;
        if !ty.ids.contains(&entry.kind) {
            return Err(NumbersError::WrongObjectType {
                id,
                expected: ty.name,
                found: entry.kind,
            });
        }
        let bytes = &self.data[entry.part][entry.range.clone()];
        let decoded = self.decoded.get().saturating_add(bytes.len() as u64);
        if decoded > self.decode_limit {
            return Err(NumbersError::TooLarge {
                what: "bytes of objects decoded (objects referred to too often)",
                limit: self.decode_limit,
            });
        }
        self.decoded.set(decoded);
        let left = self.memory_limit.saturating_sub(self.memory.get());
        let memory = wire::estimate(bytes, M::SCHEMA, left).ok_or(NumbersError::TooLarge {
            what: "bytes of memory for decoded objects",
            limit: self.memory_limit,
        })?;
        self.memory.set(self.memory.get() + memory);
        M::decode(bytes).map_err(|e| {
            damaged(
                &self.names[entry.part],
                format!("object {id} ({}) does not decode ({e})", ty.name),
            )
        })
    }
}

/// A message type and the type numbers Numbers uses for it.
pub(crate) struct ObjectType {
    pub(crate) name: &'static str,
    pub(crate) ids: &'static [u32],
}

pub(crate) mod types {
    use super::ObjectType;

    pub(crate) const DOCUMENT: ObjectType = ObjectType {
        name: "TN.DocumentArchive",
        ids: &[1],
    };
    pub(crate) const SHEET: ObjectType = ObjectType {
        name: "TN.SheetArchive",
        ids: &[2],
    };
    pub(crate) const STORAGE: ObjectType = ObjectType {
        name: "TSWP.StorageArchive",
        ids: &[2001, 2005],
    };
    pub(crate) const CALCULATION_ENGINE: ObjectType = ObjectType {
        name: "TSCE.CalculationEngineArchive",
        ids: &[4000],
    };
    pub(crate) const FORMULA_OWNER_DEPENDENCIES: ObjectType = ObjectType {
        name: "TSCE.FormulaOwnerDependenciesArchive",
        ids: &[4008],
    };
    pub(crate) const TABLE_INFO: ObjectType = ObjectType {
        name: "TST.TableInfoArchive",
        ids: &[6000],
    };
    pub(crate) const TABLE_MODEL: ObjectType = ObjectType {
        name: "TST.TableModelArchive",
        ids: &[6001],
    };
    pub(crate) const TILE: ObjectType = ObjectType {
        name: "TST.Tile",
        ids: &[6002],
    };
    pub(crate) const DATA_LIST: ObjectType = ObjectType {
        name: "TST.TableDataList",
        ids: &[6005, 6201],
    };
    pub(crate) const DATA_LIST_SEGMENT: ObjectType = ObjectType {
        name: "TST.TableDataListSegment",
        ids: &[6011],
    };
    pub(crate) const MERGE_REGION_MAP: ObjectType = ObjectType {
        name: "TST.MergeRegionMapArchive",
        ids: &[6144],
    };
    pub(crate) const RICH_TEXT_PAYLOAD: ObjectType = ObjectType {
        name: "TST.RichTextPayloadArchive",
        ids: &[6218],
    };
}

/// `pos + len`, if it fits inside `total`.
fn checked_end(pos: usize, len: u64, total: usize) -> Option<usize> {
    let len = usize::try_from(len).ok()?;
    let end = pos.checked_add(len)?;
    (end <= total).then_some(end)
}

/// Reads a protobuf varint of at most 10 bytes. Returns the value and the
/// number of bytes used.
pub(crate) fn read_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    for (i, &byte) in data.iter().take(10).enumerate() {
        let bits = u64::from(byte & 0x7f);
        if i == 9 && byte > 1 {
            return None;
        }
        value |= bits << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Joins the decompressed chunks of one `.iwa` part.
fn decompress_chunks(
    name: &str,
    data: &[u8],
    budget: &mut Budget,
) -> Result<Vec<u8>, NumbersError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let Some(header) = data.get(pos..pos + 4) else {
            return Err(damaged(name, "a chunk header is cut short"));
        };
        if header[0] != 0 {
            return Err(damaged(
                name,
                format!(
                    "a chunk starts with byte {:#04x} instead of 0x00",
                    header[0]
                ),
            ));
        }
        let len =
            usize::from(header[1]) | usize::from(header[2]) << 8 | usize::from(header[3]) << 16;
        pos += 4;
        let end = checked_end(pos, len as u64, data.len())
            .ok_or_else(|| damaged(name, "a chunk is longer than the data"))?;
        snappy::decompress(&data[pos..end], &mut out, budget).map_err(|e| match e {
            SnappyError::Budget => budget.exceeded(),
            SnappyError::Bad(detail) => damaged(name, format!("a Snappy block is bad: {detail}")),
        })?;
        pos = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints() {
        assert_eq!(read_varint(&[0x96, 0x01]), Some((150, 2)));
        assert_eq!(read_varint(&[0x80; 11]), None);
        assert_eq!(
            read_varint(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]),
            Some((u64::MAX, 10))
        );
        assert_eq!(
            read_varint(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02]),
            None
        );
    }
}
