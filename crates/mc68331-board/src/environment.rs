//! What the run gives every peripheral, beside the board file.

use std::path::Path;
use std::path::PathBuf;

#[cfg(doc)]
use crate::registry::Context;

/// What the run gives every peripheral: run-time choices that stay out of the
/// board file ([PLAN.md, Boards][plan-boards]).
///
/// It may gain fields: build one from [`Environment::default()`] and set
/// those it needs (`let mut x = Environment::default(); x.seed = ...;`).
/// The board file's directory alone is private, behind
/// [`set_board_dir`](Self::set_board_dir) and
/// [`board_dir`](Self::board_dir): a load sets it from the board file,
/// over whatever the host set, so a public field would invite a write
/// that never takes.
///
/// [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Environment {
    /// The one seed every noise model draws from.
    pub seed: u64,
    /// Where peripherals keep working copies (an EEPROM's), if the run gives
    /// one.
    pub work_dir: Option<PathBuf>,
    /// The board file's directory, which its paths are relative to: the
    /// load sets it from the board file.
    board_dir: PathBuf,
}

impl Environment {
    /// The seed of the peripheral `name`: its own stream from the one seed,
    /// so adding a peripheral does not change another's.
    #[must_use]
    pub fn seed_for(&self, name: &str) -> u64 {
        /// FNV-1a's offset basis and prime, 64-bit.
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        /// FNV-1a's prime.
        const PRIME: u64 = 0x0100_0000_01b3;
        let hash = name.bytes().fold(OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(PRIME)
        });
        mix(self.seed ^ hash)
    }

    /// Sets the directory a constructor's paths are relative to, for a test
    /// of the constructor through [`Context::new`].  A load sets its own,
    /// the board file's directory, in place of this one.
    pub fn set_board_dir(&mut self, board_dir: impl Into<PathBuf>) {
        self.board_dir = board_dir.into();
    }

    /// The directory a constructor's paths are relative to: the board
    /// file's, in a load.
    #[must_use]
    pub fn board_dir(&self) -> &Path {
        &self.board_dir
    }
}

/// `SplitMix64`'s finalizer: spreads a seed's bits.
const fn mix(seed: u64) -> u64 {
    let mixed = (seed ^ (seed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

#[cfg(test)]
mod tests {
    use super::Environment;

    #[test]
    fn each_peripheral_has_its_own_seed() {
        let environment = Environment {
            seed: 7,
            ..Environment::default()
        };
        assert_ne!(environment.seed_for("ocxo"), environment.seed_for("gps"));
        assert_eq!(environment.seed_for("ocxo"), environment.seed_for("ocxo"));
    }
}
