//! Time on the CPU's clock.

use std::fmt;
use std::ops::Add;
use std::ops::AddAssign;
use std::ops::Sub;
use std::ops::SubAssign;

/// A clock by number: system clocks since power-on, an instant on the
/// chip's time.  The board's own instant is its `Time`.
///
/// Its arithmetic is with [`Cycles`]: `+` and `-` (and `+=`, `-=`) move
/// it, saturating at power-on and the last clock; `-` of two clocks is
/// the count between them, zero if the second is later (`since`).
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
pub struct Cycle(u64);

impl Cycle {
    /// Power-on.
    pub const ZERO: Self = Self(0);
    /// The last clock there is.
    pub const MAX: Self = Self(u64::MAX);

    /// `clocks` system clocks since power-on.
    #[must_use]
    pub const fn from_clocks(clocks: u64) -> Self {
        Self(clocks)
    }

    /// Its system clocks since power-on.
    #[must_use]
    pub const fn clocks(self) -> u64 {
        self.0
    }

    /// The clocks from `earlier` to this, zero if it is later.
    #[must_use]
    pub const fn since(self, earlier: Self) -> Cycles {
        Cycles(self.0.saturating_sub(earlier.0))
    }

    /// The clocks from power-on ([`Cycle::ZERO`]) to this: its offset, as
    /// [`Cycles`] to add to another clock.
    #[must_use]
    pub const fn since_power_on(self) -> Cycles {
        Cycles(self.0)
    }
}

impl Add<Cycles> for Cycle {
    type Output = Self;

    /// `cycles` later, or the last clock.
    fn add(self, cycles: Cycles) -> Self {
        Self(self.0.saturating_add(cycles.0))
    }
}

impl Sub<Cycles> for Cycle {
    type Output = Self;

    /// `cycles` earlier, or power-on.
    fn sub(self, cycles: Cycles) -> Self {
        Self(self.0.saturating_sub(cycles.0))
    }
}

impl AddAssign<Cycles> for Cycle {
    /// `cycles` later, or the last clock.
    fn add_assign(&mut self, cycles: Cycles) {
        *self = *self + cycles;
    }
}

impl SubAssign<Cycles> for Cycle {
    /// `cycles` earlier, or power-on.
    fn sub_assign(&mut self, cycles: Cycles) {
        *self = *self - cycles;
    }
}

impl Sub for Cycle {
    type Output = Cycles;

    /// The clocks from `earlier` to this, zero if it is later.
    fn sub(self, earlier: Self) -> Cycles {
        self.since(earlier)
    }
}

/// The clocks since power-on and their unit: `12 clocks`.
impl fmt::Display for Cycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} clocks", self.0)
    }
}

/// A count of system clocks: a span of the chip's time, a cost, a
/// budget.  The CPU32RM gives every timing in clock cycles (CPU32RM 8).
/// An instant is a [`Cycle`].
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
    /// The most clocks there are.
    pub const MAX: Self = Self(u64::MAX);

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

    /// The sum, or the most clocks there are.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// The difference, or zero if `other` is more.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl Add for Cycles {
    type Output = Self;

    /// The sum, or the most clocks there are: a peripheral's wait states
    /// may be any count, so a cost never wraps.
    fn add(self, other: Self) -> Self {
        self.saturating_add(other)
    }
}

impl AddAssign for Cycles {
    /// As [`Add`].
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
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
    use super::Cycle;
    use super::Cycles;

    #[test]
    fn since_a_later_clock_is_zero() {
        assert_eq!(
            Cycle::from_clocks(4).since(Cycle::from_clocks(10)),
            Cycles::ZERO
        );
        assert_eq!(
            Cycle::from_clocks(10) - Cycle::from_clocks(4),
            Cycles::from_clocks(6)
        );
        assert_eq!(
            Cycle::from_clocks(10).since_power_on(),
            Cycles::from_clocks(10)
        );
    }

    #[test]
    fn a_clock_saturates_at_power_on_and_the_last_clock() {
        assert_eq!(Cycle::MAX + Cycles::from_clocks(1), Cycle::MAX);
        assert_eq!(Cycle::from_clocks(3) - Cycles::from_clocks(5), Cycle::ZERO);
        let mut clock = Cycle::from_clocks(3);
        clock += Cycles::from_clocks(4);
        assert_eq!(clock, Cycle::from_clocks(7));
        clock -= Cycles::from_clocks(2);
        assert_eq!(clock, Cycle::from_clocks(5));
    }

    #[test]
    fn counts_saturate_on_request() {
        let most = Cycles::MAX;
        assert_eq!(most.clocks(), u64::MAX);
        assert_eq!(most.saturating_add(Cycles::from_clocks(1)), most);
        assert_eq!(
            Cycles::from_clocks(3).saturating_sub(Cycles::from_clocks(5)),
            Cycles::ZERO
        );
    }

    #[test]
    fn cycles_show_their_unit() {
        assert_eq!(Cycles::from_clocks(12).to_string(), "12 clocks");
        assert_eq!(Cycle::from_clocks(12).to_string(), "12 clocks");
        assert_eq!(Cycles::from_clocks(3).clocks(), 3);
    }
}
