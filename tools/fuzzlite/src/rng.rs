//! A small deterministic random generator, so a run repeats from its seed.

/// SplitMix64: fast, good enough for choosing mutations, and the same on
/// every platform.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`, or 0 when `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// True once in `n` times.
    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    pub fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }

    /// One element of a non-empty slice.
    pub fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    /// A count that is usually small and sometimes large, for repeats.
    pub fn count(&mut self, max: usize) -> usize {
        let n = match self.below(4) {
            0 => 2,
            1 => 1 + self.below(16),
            2 => 1 + self.below(512),
            _ => 1 + self.below(65_536),
        };
        n.min(max.max(1))
    }
}

/// FNV-1a, for naming finding files by their content.
pub fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}
