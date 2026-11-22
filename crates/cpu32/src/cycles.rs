//! Time on the CPU's clock.

use std::fmt;
use std::ops::Add;
use std::ops::AddAssign;
use std::ops::Sub;

/// A count of CPU clock cycles: a time on the board's clock, or a span
/// of it.  The CPU32RM gives every timing in clock cycles (CPU32RM 8).
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Cycles(u64);

impl Cycles {
    /// No time.
    pub const ZERO: Self = Self(0);

    /// `clocks` clock cycles.
    #[must_use]
    pub const fn from_clocks(clocks: u64) -> Self {
        Self(clocks)
    }

    /// Its clock cycles.
    #[must_use]
    pub const fn clocks(self) -> u64 {
        self.0
    }

    /// The span until `later`, zero if it has passed.
    #[must_use]
    pub const fn until(self, later: Self) -> Self {
        Self(later.0.saturating_sub(self.0))
    }
}

impl Add for Cycles {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl AddAssign for Cycles {
    fn add_assign(&mut self, other: Self) {
        self.0 += other.0;
    }
}

impl Sub for Cycles {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

/// The count and its unit: `12 clocks`.
impl fmt::Display for Cycles {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} clocks", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::Cycles;

    #[test]
    fn until_a_time_passed_is_zero() {
        assert_eq!(
            Cycles::from_clocks(10).until(Cycles::from_clocks(4)),
            Cycles::ZERO
        );
        assert_eq!(
            Cycles::from_clocks(4).until(Cycles::from_clocks(10)),
            Cycles::from_clocks(6)
        );
    }

    #[test]
    fn cycles_show_their_unit() {
        assert_eq!(Cycles::from_clocks(12).to_string(), "12 clocks");
        assert_eq!(Cycles::from_clocks(3).clocks(), 3);
    }
}
