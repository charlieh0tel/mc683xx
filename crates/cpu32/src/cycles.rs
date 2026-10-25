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
pub struct Cycles(pub u64);

impl Cycles {
    /// No time.
    pub const ZERO: Self = Self(0);

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

impl From<u64> for Cycles {
    fn from(clocks: u64) -> Self {
        Self(clocks)
    }
}

impl From<Cycles> for u64 {
    fn from(cycles: Cycles) -> Self {
        cycles.0
    }
}

#[cfg(test)]
mod tests {
    use super::Cycles;

    #[test]
    fn until_a_time_passed_is_zero() {
        assert_eq!(Cycles(10).until(Cycles(4)), Cycles::ZERO);
        assert_eq!(Cycles(4).until(Cycles(10)), Cycles(6));
    }

    #[test]
    fn cycles_show_their_unit() {
        assert_eq!(Cycles(12).to_string(), "12 clocks");
        assert_eq!(u64::from(Cycles::from(3)), 3);
    }
}
