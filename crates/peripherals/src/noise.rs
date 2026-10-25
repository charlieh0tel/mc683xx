//! Modeled noise: a fixed sequence from a seed, so a run with the same
//! seed is the same each time, on any host.

use serde::Deserialize;
use serde::Serialize;

/// `SplitMix64`'s increment, the golden ratio's fraction in 64 bits.
const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
/// `SplitMix64`'s first multiplier.
const FIRST_MULTIPLIER: u64 = 0xbf58_476d_1ce4_e5b9;
/// `SplitMix64`'s second multiplier.
const SECOND_MULTIPLIER: u64 = 0x94d0_49bb_1331_11eb;
/// The bits of an `f64`'s significand, its precision.
const SIGNIFICAND_BITS: u32 = f64::MANTISSA_DIGITS;
/// The uniforms summed for a normal: twelve, whose sum's variance is
/// one (each uniform's is 1/12).
const UNIFORMS_PER_NORMAL: u32 = 12;

/// A generator of noise: `SplitMix64` for uniforms, and for normals the
/// sum of twelve of them less their mean (the Irwin-Hall
/// distribution), within six of the mean.  The sum needs only exact
/// IEEE arithmetic, where Box-Muller's logarithm and cosine come from
/// the host's math library and may differ in their last bit between
/// hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Noise {
    /// `SplitMix64`'s state.
    state: u64,
}

impl Noise {
    /// The generator from `seed`.
    pub(crate) const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// A normal value, mean 0 and standard deviation 1.
    pub(crate) fn normal(&mut self) -> f64 {
        let sum: f64 = (0..UNIFORMS_PER_NORMAL).map(|_| self.uniform()).sum();
        sum - f64::from(UNIFORMS_PER_NORMAL) / 2.0
    }

    /// A uniform value in [0, 1).
    fn uniform(&mut self) -> f64 {
        self.state = self.state.wrapping_add(GAMMA);
        let mixed = (self.state ^ (self.state >> 30)).wrapping_mul(FIRST_MULTIPLIER);
        let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(SECOND_MULTIPLIER);
        let mixed = mixed ^ (mixed >> 31);
        let significand = mixed >> (u64::BITS - SIGNIFICAND_BITS);
        significand as f64 / (1_u64 << SIGNIFICAND_BITS) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::Noise;

    #[test]
    fn a_seed_gives_its_own_sequence() {
        let draws = |seed| {
            let mut noise = Noise::new(seed);
            (0..16).map(|_| noise.normal()).collect::<Vec<_>>()
        };
        assert_eq!(draws(1), draws(1));
        assert_ne!(draws(1), draws(2));
    }

    #[test]
    fn normals_have_mean_zero_and_deviation_one() {
        const DRAWS: u32 = 20_000;
        let mut noise = Noise::new(7);
        let draws: Vec<f64> = (0..DRAWS).map(|_| noise.normal()).collect();
        let mean = draws.iter().sum::<f64>() / f64::from(DRAWS);
        let variance = draws.iter().map(|draw| draw * draw).sum::<f64>() / f64::from(DRAWS);
        assert!(mean.abs() < 0.03, "mean {mean}");
        assert!((variance - 1.0).abs() < 0.05, "variance {variance}");
        assert!(draws.iter().all(|draw| draw.abs() <= 6.0));
    }
}
