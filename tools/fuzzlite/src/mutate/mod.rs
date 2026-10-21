//! The mutation engine: generic byte mutations plus format-aware ones that
//! aim at the shapes that made readers blow up before (record lengths,
//! CONTINUE chains, shared-formula ranges, SST counts, sector chains,
//! directory entries, decompression, huge CSV fields).
//!
//! Everything is driven by one [`Rng`], so a run repeats from its seed.

mod biff;
mod bytes;
mod cfb;
mod container;
mod csv;
mod ovba;

pub use container::Container;

use crate::rng::Rng;

/// A family of format-aware mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// CSV text: quotes, delimiters, line breaks, encodings and BOMs.
    Csv,
    /// BIFF records in the workbook stream (inside OLE2 or raw).
    Biff,
    /// The OLE2 compound file itself: header, FAT, directory, mini stream.
    Cfb,
    /// A VBA project: the `dir` stream, compressed containers, `PROJECT`.
    Ovba,
}

/// A seed input, with its compound file parsed once.
#[derive(Debug, Clone)]
pub struct Seed {
    /// Where it came from, relative to the workspace root.
    pub name: String,
    pub bytes: Vec<u8>,
    /// The streams, when the seed is an OLE2 file that parses cleanly.
    pub container: Option<Container>,
}

impl Seed {
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Seed {
        let container = Container::parse(&bytes);
        Seed {
            name: name.into(),
            bytes,
            container,
        }
    }
}

/// One mutated input and the mutations that made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutant {
    pub bytes: Vec<u8>,
    pub ops: Vec<&'static str>,
}

impl Mutant {
    /// The set of format-aware mutations used, sorted and joined with `+`
    /// (order and repeats do not matter), or `bytes` when there were none.
    pub fn family(&self) -> String {
        let mut aware: Vec<&str> = self
            .ops
            .iter()
            .copied()
            .filter(|op| !op.starts_with("bytes:"))
            .collect();
        aware.sort_unstable();
        aware.dedup();
        if aware.is_empty() {
            "bytes".into()
        } else {
            aware.join("+")
        }
    }
}

/// Makes mutants from seeds.
#[derive(Debug, Clone)]
pub struct Mutator {
    rng: Rng,
    shapes: Vec<Shape>,
    max_len: usize,
}

impl Mutator {
    /// `shapes` chooses the format-aware mutations; `max_len` caps the size
    /// of a mutant.
    pub fn new(seed: u64, shapes: &[Shape], max_len: usize) -> Mutator {
        Mutator {
            rng: Rng::new(seed),
            shapes: shapes.to_vec(),
            max_len,
        }
    }

    /// The generator, for choosing seeds with the same stream.
    pub fn rng(&mut self) -> &mut Rng {
        &mut self.rng
    }

    /// Mutates `seed`; `other` is the partner for splicing.
    pub fn mutate(&mut self, seed: &Seed, other: &Seed) -> Mutant {
        let mut ops = Vec::new();
        let mut out = None;
        if !self.shapes.is_empty() && !self.rng.one_in(3) {
            let shape = self.rng.pick(&self.shapes);
            out = self.aware(shape, seed, &mut ops);
        }
        let aware = out.is_some();
        let mut bytes = out.unwrap_or_else(|| seed.bytes.clone());
        // After a format-aware mutation, often leave the rest intact so the
        // reader gets past its first checks.
        let generic = if aware {
            self.rng.below(3)
        } else {
            1 + self.rng.below(4)
        };
        for _ in 0..generic {
            ops.push(bytes::mutate(
                &mut self.rng,
                &mut bytes,
                &other.bytes,
                self.max_len,
            ));
        }
        bytes.truncate(self.max_len);
        Mutant { bytes, ops }
    }

    fn aware(&mut self, shape: Shape, seed: &Seed, ops: &mut Vec<&'static str>) -> Option<Vec<u8>> {
        let rng = &mut self.rng;
        let (bytes, op) = match shape {
            Shape::Csv => csv::mutate(rng, &seed.bytes, self.max_len),
            Shape::Cfb => cfb::mutate(rng, &seed.bytes)?,
            Shape::Biff => {
                let (bytes, stacked) = biff::mutate(rng, seed, self.max_len)?;
                ops.extend(stacked);
                return Some(bytes);
            }
            Shape::Ovba => {
                let (bytes, stacked) = ovba::mutate(rng, seed.container.as_ref()?, self.max_len)?;
                ops.extend(stacked);
                return Some(bytes);
            }
        };
        ops.push(op);
        Some(bytes)
    }
}

/// Little-endian helpers shared by the format modules. Reads past the end
/// give `None`; writes past the end do nothing.
pub(crate) mod le {
    pub fn u16_at(b: &[u8], at: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            b.get(at..at.checked_add(2)?)?.try_into().ok()?,
        ))
    }

    pub fn u32_at(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            b.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    }

    pub fn u64_at(b: &[u8], at: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            b.get(at..at.checked_add(8)?)?.try_into().ok()?,
        ))
    }

    pub fn put(b: &mut [u8], at: usize, value: &[u8]) {
        if let Some(end) = at.checked_add(value.len())
            && let Some(slot) = b.get_mut(at..end)
        {
            slot.copy_from_slice(value);
        }
    }

    pub fn put_u16(b: &mut [u8], at: usize, v: u16) {
        put(b, at, &v.to_le_bytes());
    }

    pub fn put_u32(b: &mut [u8], at: usize, v: u32) {
        put(b, at, &v.to_le_bytes());
    }

    pub fn put_u64(b: &mut [u8], at: usize, v: u64) {
        put(b, at, &v.to_le_bytes());
    }
}

/// Interesting 32-bit values: edges, signed edges, and lengths.
pub(crate) const INTERESTING_U32: &[u32] = &[
    0,
    1,
    2,
    7,
    8,
    16,
    0x7F,
    0x80,
    0xFF,
    0x100,
    0x1000,
    0x2020,
    0x7FFF,
    0x8000,
    0xFFFF,
    0x1_0000,
    0x10_0000,
    0x100_0000,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFA,
    0xFFFF_FFFE,
    0xFFFF_FFFF,
];

/// Interesting 16-bit values.
pub(crate) const INTERESTING_U16: &[u16] = &[
    0, 1, 2, 0x7F, 0x80, 0xFF, 0x100, 0x2020, 0x2021, 0x3FFF, 0x7FFF, 0x8000, 0xFFFE, 0xFFFF,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn seeds() -> (Seed, Seed) {
        (
            Seed::new("a", b"name,city\r\nAda,\"Z\xc3\xbcrich\"\r\n".to_vec()),
            Seed::new("b", (0u8..=255).collect()),
        )
    }

    fn run(seed: u64, shapes: &[Shape]) -> Vec<Mutant> {
        let (a, b) = seeds();
        let mut m = Mutator::new(seed, shapes, 1 << 20);
        (0..300).map(|_| m.mutate(&a, &b)).collect()
    }

    #[test]
    fn same_seed_same_mutants() {
        for shapes in [&[][..], &[Shape::Csv], &[Shape::Biff, Shape::Cfb]] {
            assert_eq!(run(7, shapes), run(7, shapes));
        }
    }

    #[test]
    fn other_seed_other_mutants() {
        assert_ne!(run(7, &[Shape::Csv]), run(8, &[Shape::Csv]));
    }

    #[test]
    fn mutants_change_the_input_and_respect_max_len() {
        let (a, b) = seeds();
        let mut m = Mutator::new(1, &[Shape::Csv], 4096);
        let mut changed = 0;
        for _ in 0..500 {
            let out = m.mutate(&a, &b);
            assert!(out.bytes.len() <= 4096);
            assert!(!out.ops.is_empty());
            changed += usize::from(out.bytes != a.bytes);
        }
        assert!(changed > 450, "only {changed} of 500 mutants differ");
    }

    #[test]
    fn every_csv_and_byte_mutation_shows_up() {
        let names: std::collections::BTreeSet<&str> = run(3, &[Shape::Csv])
            .iter()
            .flat_map(|m| m.ops.clone())
            .collect();
        for want in [
            "bytes:flip",
            "bytes:dup",
            "bytes:splice",
            "csv:huge_quoted",
            "csv:bom",
        ] {
            assert!(names.contains(want), "{want} not in {names:?}");
        }
    }

    #[test]
    fn family_is_the_set_of_aware_mutations() {
        let m = |ops: &[&'static str]| Mutant {
            bytes: Vec::new(),
            ops: ops.to_vec(),
        };
        let a = m(&["ovba:bomb_all", "bytes:flip", "ovba:code_page"]);
        let b = m(&["ovba:code_page", "ovba:bomb_all", "ovba:bomb_all"]);
        assert_eq!(a.family(), "ovba:bomb_all+ovba:code_page");
        assert_eq!(a.family(), b.family());
        assert_eq!(m(&["bytes:flip"]).family(), "bytes");
    }

    #[test]
    fn le_helpers_ignore_out_of_range() {
        let mut b = [0u8; 4];
        le::put_u32(&mut b, 2, 0xFFFF_FFFF);
        assert_eq!(b, [0; 4]);
        assert_eq!(le::u16_at(&b, 3), None);
        assert_eq!(le::u32_at(&b, usize::MAX), None);
        le::put_u16(&mut b, 2, 0xABCD);
        assert_eq!(le::u16_at(&b, 2), Some(0xABCD));
    }
}
