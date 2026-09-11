#![allow(clippy::arithmetic_side_effects)] // xorshift shifts and the bounded modulo on u64

pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

pub(crate) fn rotation_bytes() -> Vec<u8> {
    (0u8..128)
        .filter(|byte| {
            let first = byte & 0b11;
            let second = (byte >> 2) & 0b11;
            first != 0b11 && second != 0b11 && first != second
        })
        .collect()
}
