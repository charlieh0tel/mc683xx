//! A small seeded generator, so a differential run is the same run
//! every time from the same seed.

/// `SplitMix64`'s increment: 2^64 over the golden ratio.
const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
/// `SplitMix64`'s first finalizer multiplier.
const MIX_FIRST: u64 = 0xbf58_476d_1ce4_e5b9;
/// `SplitMix64`'s second finalizer multiplier.
const MIX_SECOND: u64 = 0x94d0_49bb_1331_11eb;

/// `SplitMix64`'s finalizer: a value whose bits all depend on all of
/// `value`'s.
#[must_use]
pub const fn mix(value: u64) -> u64 {
    let value = (value ^ (value >> 30)).wrapping_mul(MIX_FIRST);
    let value = (value ^ (value >> 27)).wrapping_mul(MIX_SECOND);
    value ^ (value >> 31)
}

/// The `SplitMix64` generator.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    /// The state, advanced by `GAMMA` each draw.
    state: u64,
}

impl SplitMix64 {
    /// A generator from `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 bits.
    pub const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GAMMA);
        mix(self.state)
    }

    /// The next 32 bits.
    pub const fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> u32::BITS) as u32
    }

    /// Whether a one-in-`odds` chance came up.
    pub const fn one_in(&mut self, odds: u64) -> bool {
        self.next_u64().is_multiple_of(odds)
    }
}

/// `mix` of `seed` and `index` together: the `index`th value of a
/// stream with no state, for data addressed rather than drawn.
#[must_use]
pub const fn mix_at(seed: u64, index: u64) -> u64 {
    mix(seed ^ index.wrapping_mul(GAMMA))
}

#[cfg(test)]
mod tests {
    use super::SplitMix64;

    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let mut first = SplitMix64::new(42);
        let mut second = SplitMix64::new(42);
        assert_eq!(first.next_u64(), second.next_u64());
        assert_ne!(first.next_u64(), SplitMix64::new(43).next_u64());
    }
}
